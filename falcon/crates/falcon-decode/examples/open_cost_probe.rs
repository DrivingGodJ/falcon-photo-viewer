//! Read-only stage probe; no application, GPU, review writes or disk-cache eviction.
//! Reports decoder cost separately from the scan. Contention is a controlled stress
//! comparison, not a reproduction of the native scheduler or a cold-start benchmark.
use falcon_decode::{browse_frame_rgba, Lane, Shot};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Barrier,
};
use std::time::Instant;

fn thumb(shot: &Shot) -> u128 {
    let start = Instant::now();
    let f = browse_frame_rgba(shot, 256, true, Lane::Thumb).expect("thumbnail decode");
    let _ = falcon_decode::frame_source_gamut(shot, f.source);
    let _ = falcon_decode::read_orientation(shot, false);
    start.elapsed().as_micros()
}

fn batch(shots: &[Shot], competitors: usize) {
    let next = AtomicUsize::new(0);
    let barrier = Barrier::new(4 + competitors);
    let start = Instant::now();
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..4 {
            workers.push(scope.spawn(|| {
                barrier.wait();
                let mut times = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(s) = shots.get(i) else {
                        break;
                    };
                    times.push(thumb(s));
                }
                times
            }));
        }
        let mut prefetch = Vec::new();
        for i in 0..competitors {
            let barrier = &barrier;
            prefetch.push(scope.spawn(move || {
                barrier.wait();
                for round in 0..2 {
                    let s = &shots[(i + round) % shots.len()];
                    let _ = browse_frame_rgba(s, 2560, true, Lane::Fast).expect("fast decode");
                }
            }));
        }
        let mut times: Vec<u128> = workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect();
        let visible_done = start.elapsed().as_micros();
        times.sort_unstable();
        println!("thumb_batch count={} workers=4 fast_competitors={} visible_done_us={} median_job_us={} max_job_us={}",
            shots.len(), competitors, visible_done, times[times.len()/2], times[times.len()-1]);
        for w in prefetch {
            w.join().unwrap();
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: open_cost_probe <folder> <filename>")?,
    );
    let target = dir.join(std::env::args_os().nth(2).ok_or("filename required")?);
    let mut scan = falcon_decode::scan_folder_with_metadata(&dir)?;
    // This folder uses Modified descending. Tie by canonical name for repeatability.
    scan.shots.sort_by(|a, b| {
        let key = |s: &Shot| {
            s.jpg
                .as_ref()
                .and_then(|p| scan.metadata.get(p))
                .and_then(|m| m.modified().ok())
        };
        key(b).cmp(&key(a)).then_with(|| a.name.cmp(&b.name))
    });
    let current = scan
        .shots
        .iter()
        .position(|s| s.jpg.as_ref() == Some(&target))
        .ok_or("target not found")?;
    let count = scan.shots.len().min(21);
    let base = current.saturating_sub(10).min(scan.shots.len() - count);
    let samples: Vec<_> = scan.shots[base..base + count]
        .iter()
        .filter(|s| !s.cloud_placeholder && s.kind == falcon_decode::SrcKind::Png)
        .cloned()
        .collect();
    if samples.is_empty() {
        return Err("no local PNG samples".into());
    }
    println!(
        "scan shots={} enumerate_ms={} headers_ms={} finish_ms={} sample_count={}",
        scan.shots.len(),
        scan.enumerate_ms,
        scan.headers_ms,
        scan.finish_ms,
        samples.len()
    );
    for s in samples.iter().take(3) {
        let start = Instant::now();
        let (rgb, w, h) = falcon_decode::decode_full_rgb(s)?;
        let decode_us = start.elapsed().as_micros();
        let start = Instant::now();
        let _ = falcon_decode::finish_fast_rgba_timed(rgb, w, h, 256)?;
        let resize_us = start.elapsed().as_micros();
        let start = Instant::now();
        let _ = falcon_decode::shot_source_gamut(s);
        let color_us = start.elapsed().as_micros();
        let start = Instant::now();
        let _ = falcon_decode::read_orientation(s, false);
        println!("sample={} source={}x{} full_rgb_decode_us={} resize_rgba_256_us={} color_probe_us={} orientation_probe_us={}", s.name,w,h,decode_us,resize_us,color_us,start.elapsed().as_micros());
    }
    batch(&samples, 0);
    batch(&samples, 18);
    batch(&samples, 0);
    Ok(())
}
