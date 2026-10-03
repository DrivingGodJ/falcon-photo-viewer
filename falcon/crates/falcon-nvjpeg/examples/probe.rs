//! Probe: scaled GPU decode of the real 45 MP JPG via [`falcon_nvjpeg`], vs the
//! pure-Rust CPU decoder. Prints dims + per-decode timing for the Reference (1/2)
//! and Fast (1/4) targets, and saves a small PNG so the colour can be eyeballed.
//! Verifies the §17.3 scaled-dimension rounding against the source dimensions.
//!
//!   cargo run -p falcon-nvjpeg --example probe   (needs <CUDA>\bin\x64 reachable)

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

use std::io::Cursor;
use std::time::Instant;

use falcon_nvjpeg::NvjpegContext;

const OUT: &str = r"./probe-output/nvjpeg_scaled_out.png";

const FRAME_DIM: u32 = 4096; // Reference target
const SCRUB_DIM: u32 = 2048; // Fast target

fn save_png(rgb: &[u8], w: u32, h: u32) {
    let Some(img) = image::RgbImage::from_raw(w, h, rgb.to_vec()) else {
        eprintln!("save_png: RGB buffer is not {w}x{h}*3");
        return;
    };
    let nh = (1280u64 * h as u64 / w as u64).max(1) as u32;
    let small = image::imageops::resize(&img, 1280, nh, image::imageops::FilterType::Triangle);
    std::fs::create_dir_all(std::path::Path::new(OUT).parent().unwrap()).ok();
    match small.save(OUT) {
        Ok(()) => println!("saved {OUT}"),
        Err(e) => eprintln!("save_png failed: {e}"),
    }
}

fn main() {
    let jpeg = std::fs::read(fixture_paths::benchmark_jpeg()).expect("read test jpg");
    println!("== nvJPEG scaled decode vs CPU ==\nfile: {} MB\n", jpeg.len() / 1_000_000);

    // CPU baseline (jpeg-decoder is multithreaded via rayon internally).
    let n_cpu = 5;
    let (mut cw, mut ch) = (0u16, 0u16);
    let t = Instant::now();
    for _ in 0..n_cpu {
        let mut d = jpeg_decoder::Decoder::new(Cursor::new(&jpeg));
        let _ = d.decode().expect("cpu decode");
        let i = d.info().unwrap();
        cw = i.width;
        ch = i.height;
    }
    let cpu = t.elapsed().as_secs_f64() * 1e3 / n_cpu as f64;
    println!("CPU jpeg-decoder (full {cw}x{ch}): {cpu:7.1} ms");

    let Some(mut ctx) = NvjpegContext::new() else {
        println!("\nnvJPEG unavailable (no CUDA / hardware backend) — CPU fallback would be used.");
        return;
    };

    for (label, target) in [("Reference 1/2", FRAME_DIM), ("Fast 1/4", SCRUB_DIM)] {
        // Warm-up (also confirms the hardware backend accepts this bitstream).
        let Some((rgb, w, h)) = ctx.decode_scaled(&jpeg, target) else {
            println!("nvJPEG {label}: unsupported bitstream (would fall back to CPU)");
            continue;
        };
        assert_eq!(rgb.len(), w as usize * h as usize * 3, "tight-packed RGB");
        // Rounding check vs the source dims (ceil division by the scale factor).
        let factor = (cw as u32).max(ch as u32) / w.max(h);
        println!(
            "  source/{factor} → {w}x{h}  (expected ceil: {}x{})",
            (cw as u32).div_ceil(factor.max(1)),
            (ch as u32).div_ceil(factor.max(1)),
        );

        let n = 30;
        let t = Instant::now();
        let mut last = (rgb, w, h);
        for _ in 0..n {
            last = ctx.decode_scaled(&jpeg, target).expect("decode");
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        println!(
            "nvJPEG {label} ({}x{}): {ms:7.1} ms  {:.1}x vs CPU, {:.0} img/s",
            last.1,
            last.2,
            cpu / ms,
            1000.0 / ms,
        );
        if target == FRAME_DIM {
            save_png(&last.0, last.1, last.2);
        }
    }

    println!("\n30 fps Fast mode needs <= 33 ms/frame; Reference target ~40 ms.");
}
