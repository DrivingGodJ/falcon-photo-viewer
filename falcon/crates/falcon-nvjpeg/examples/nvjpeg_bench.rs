//! Time nvJPEG (GPU) decode of a JPEG at full + scaled resolutions, to compare against the
//! CPU decoder for the adaptive-hi-res full-source decode. Usage:
//! `cargo run --release --example nvjpeg_bench -- <file>`

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: nvjpeg_bench <file>");
    let bytes = std::fs::read(&path).expect("read");
    let mut ctx = match falcon_nvjpeg::NvjpegContext::new() {
        Some(c) => c,
        None => {
            println!("nvJPEG unavailable (no CUDA) — CPU path only");
            return;
        }
    };
    let _ = ctx.decode_scaled(&bytes, u32::MAX); // warm
    for long in [u32::MAX, 8192u32, 3840u32] {
        let t = Instant::now();
        match ctx.decode_scaled(&bytes, long) {
            Some((_, w, h)) => {
                println!("nvjpeg long={long:<6} -> {w}×{h} in {} ms", t.elapsed().as_millis())
            }
            None => println!("nvjpeg long={long:<6} -> unsupported"),
        }
    }
}
