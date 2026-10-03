//! Read-only synthetic quality/performance probe using the production kernels.
#[path = "../src/glass_blur.rs"]
mod glass_blur;
use std::time::Instant;

fn main() {
    let src: Vec<u8> = (0..64)
        .flat_map(|_| {
            (0..512).flat_map(|x| {
                if x < 247 {
                    [64, 64, 64, 255]
                } else {
                    [192, 192, 192, 255]
                }
            })
        })
        .collect();
    for (name, resize) in [
        (
            "Lanczos frost",
            falcon_decode::downscale_rgba as fn(&[u8], u32, u32, u32) -> (Vec<u8>, u32, u32),
        ),
        ("Area frost", falcon_decode::downscale_frost_rgba),
    ] {
        let (rgba, _, _) = resize(&src, 512, 64, 160);
        let range = rgba
            .chunks_exact(4)
            .map(|p| p[0])
            .fold((255, 0), |(lo, hi), v| (lo.min(v), hi.max(v)));
        println!("{name}: step input=[64,192] output={range:?}");
    }
    let (w, h) = (160u32, 512u32);
    let source: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            [
                (i % 239) as u8,
                ((i * 7) % 251) as u8,
                ((i * 13) % 253) as u8,
                255,
            ]
        })
        .collect();
    for (name, kernel) in [
        ("box_radius6", 0),
        ("box_radius18", 1),
        ("gaussian_sigma7", 2),
    ] {
        let mut times = Vec::new();
        for _ in 0..110 {
            let mut p = source.clone();
            let at = Instant::now();
            match kernel {
                0 => glass_blur::box_blur(&mut p, w, h, 6),
                1 => glass_blur::box_blur(&mut p, w, h, 18),
                _ => glass_blur::gaussian_blur(&mut p, w, h, 7.),
            }
            let us = at.elapsed().as_micros();
            std::hint::black_box(p);
            times.push(us);
        }
        times.drain(..10);
        times.sort_unstable();
        println!(
            "{name}: canvas={w}x{h} median_us={} p95_us={} max_us={}",
            times[50], times[95], times[99]
        );
    }
}
