//! Exercise falcon-decode on the real test folder end-to-end: scan/pair, EXIF,
//! fast/reference/thumbnail/develop frames, plus parallel sweeps that mimic the
//! filmstrip prefetch and the scrub path. Writes output JPEGs to eyeball color.
//!
//!   cargo run -p falcon-decode --example probe

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

use std::path::Path;
use std::time::Instant;

use falcon_decode::*;
use rayon::prelude::*;

const OUT: &str = r"./probe-output";

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    std::fs::create_dir_all(OUT).ok();

    let shots = scan_folder(Path::new(fixture_paths::photos())).expect("scan folder");
    let pairs = shots.iter().filter(|s| s.has_raw && s.has_jpg).count();
    println!(
        "scan: {} shots ({} RAW+JPG, {} raw-only, {} jpg-only)",
        shots.len(),
        pairs,
        shots.iter().filter(|s| s.has_raw && !s.has_jpg).count(),
        shots.iter().filter(|s| !s.has_raw && s.has_jpg).count(),
    );

    let s0 = &shots[0];
    println!("\nshot[0] = {} (raw={}, jpg={})", s0.name, s0.has_raw, s0.has_jpg);
    println!("EXIF: {:#?}", read_exif(s0));

    let t = Instant::now();
    let f = fast_frame(s0, 4096).expect("fast");
    println!("\nfast_frame      : {:>4} KB  {:>6.0} ms", f.bytes.len() / 1024, ms(t));
    std::fs::write(format!("{OUT}/fast.jpg"), &f.bytes).ok();

    let t = Instant::now();
    let r = reference_frame(s0, 4096).expect("reference");
    println!("reference_frame : {:>4} KB  {:>6.0} ms", r.bytes.len() / 1024, ms(t));
    std::fs::write(format!("{OUT}/reference.jpg"), &r.bytes).ok();

    let t = Instant::now();
    let th = thumbnail(s0, 320).expect("thumb");
    println!("thumbnail       : {:>4} KB  {:>6.0} ms", th.bytes.len() / 1024, ms(t));
    std::fs::write(format!("{OUT}/thumb.jpg"), &th.bytes).ok();

    // Parallel thumbnail sweep — the filmstrip warm-up.
    let t = Instant::now();
    let n = shots.par_iter().filter_map(|s| thumbnail(s, 320).ok()).count();
    let secs = t.elapsed().as_secs_f64();
    println!("\nthumb sweep : {n}/{} in {secs:.2}s -> {:.0} thumb/s", shots.len(), n as f64 / secs);

    // Parallel fast-frame sweep — the scrub path throughput.
    let t = Instant::now();
    let n = shots.par_iter().filter_map(|s| fast_frame(s, 4096).ok()).count();
    let secs = t.elapsed().as_secs_f64();
    println!("fast  sweep : {n}/{} in {secs:.2}s -> {:.0} img/s", shots.len(), n as f64 / secs);

    // One true raw develop — the on-demand "Develop RAW" path.
    if let Some(raw_shot) = shots.iter().find(|s| s.has_raw) {
        let t = Instant::now();
        match develop_raw(raw_shot, 4096) {
            Ok(d) => {
                println!(
                    "\ndevelop_raw({}) : {} KB in {:.2}s",
                    raw_shot.name,
                    d.bytes.len() / 1024,
                    t.elapsed().as_secs_f64()
                );
                std::fs::write(format!("{OUT}/develop.jpg"), &d.bytes).ok();
            }
            Err(e) => println!("\ndevelop_raw failed: {e:?}"),
        }
    }

    println!("\noutputs in {OUT}");
}
