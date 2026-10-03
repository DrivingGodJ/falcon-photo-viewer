//! v0.8.145 (E2) — the YUV kernel's DIAGNOSTICS HOOK. Not shipped, not on any decode path; this is
//! an example binary, so nothing links it into the app.
//!
//! Two jobs the `cargo test` suite deliberately does not do, because both need inputs that cannot
//! live in the repository:
//!
//!   1. **Full-tile digests.** The pinned rows in `GOLDEN_PINS` use 256×256 real-tile excerpts
//!      (98 KB each) rather than whole 896×1024 tiles (1.31 MB each). This computes the whole-tile
//!      digest so the number exists and can be re-derived.
//!   2. **The WIC delta.** The kernel's RGB for one real tile against the SHIPPING WIC decoder's
//!      RGB for the same region of the same file — the honest number for the architect's E5 ruling
//!      on acceptability. Differences are EXPECTED (WIC's chroma filter is its own, and undocumented).
//!
//! ## Producing an NV12 input
//!
//! ```text
//! # 1. rebuild a tile's Annex-B bitstream (parameter sets + the one IDR_N_LP slice) with the
//! #    Stage-0 spike tooling in %LOCALAPPDATA%\Falcon\heic_spike\ (heicparse.annexb_from_hvcc +
//! #    annexb_from_item), then:
//! ffmpeg -v error -y -i tile.hevc -f rawvideo -pix_fmt yuv420p tile.yuv420p
//! #    (rawvideo yuv420p is a COPY out of the decoder — no swscale stage, so no silent range or
//! #     colour rescale. Interleave the U and V planes to get NV12. ffmpeg's -hwaccel flags are
//! #     known-broken on this box and silently fall back to software; that is fine here, since
//! #     Stage 0 measured NVDEC and libavcodec to be byte-identical on these very tiles.)
//! ```
//!
//! ## Usage
//!
//! ```text
//! cargo run -p falcon-decode --example yuv_kernel_probe -- \
//!     --nv12 <path> --dims <W>x<H> [--matrix 601|709] [--range full|limited] [--siting left|center]
//!     [--dump <out.rgb>]
//!     [--wic <file.heic> --at <X>,<Y>]     # compare against the shipping decoder at that origin
//! ```
//!
//! With no arguments it prints the whole `GOLDEN_PINS` table — the same digests `cargo test` asserts.

use std::path::{Path, PathBuf};

use falcon_decode::yuv_kernel::*;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let Some(nv12) = arg(&args, "--nv12") else {
        println!("no --nv12 given; printing the pinned golden table instead\n");
        println!("{:<40} {:>4}x{:<4} {:<28} sha256", "case", "w", "h", "params");
        for c in GOLDEN_PINS {
            println!(
                "{:<40} {:>4}x{:<4} {:<28} {}",
                c.name,
                c.w,
                c.h,
                format!("{:?}/{:?}/{:?}", c.params.matrix, c.params.range, c.params.siting),
                c.digest
            );
        }
        println!("\n(the CPU harness asserts these; falcon-gpu's twin asserts the same set)");
        return;
    };

    let dims = arg(&args, "--dims").expect("--dims WxH is required with --nv12");
    let (ws, hs) = dims.split_once('x').expect("--dims must look like 896x1024");
    let (w, h) = (ws.parse::<u32>().unwrap(), hs.parse::<u32>().unwrap());
    let matrix = match arg(&args, "--matrix").as_deref().unwrap_or("601") {
        "601" => YuvMatrix::Bt601,
        "709" => YuvMatrix::Bt709,
        o => panic!("--matrix must be 601 or 709, got {o}"),
    };
    let range = match arg(&args, "--range").as_deref().unwrap_or("full") {
        "full" => YuvRange::Full,
        "limited" => YuvRange::Limited,
        o => panic!("--range must be full or limited, got {o}"),
    };
    let siting = match arg(&args, "--siting").as_deref().unwrap_or("left") {
        "left" => ChromaSiting::Left,
        "center" | "centre" => ChromaSiting::Center,
        o => panic!("--siting must be left or center, got {o}"),
    };
    let params = YuvParams { matrix, range, siting };

    let raw = std::fs::read(&nv12).expect("read the NV12 file");
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let want = (w * h + 2 * cw * ch) as usize;
    assert_eq!(raw.len(), want, "{nv12} is {} bytes; {w}x{h} NV12 needs {want}", raw.len());
    let (y, uv) = raw.split_at((w * h) as usize);

    let f = Nv12Frame::packed(y, uv, w, h);
    let t = std::time::Instant::now();
    let rgb = nv12_to_rgb8(&f, params).expect("convert");
    let ms = t.elapsed().as_secs_f64() * 1e3;

    println!("kernel  : {nv12}");
    println!("  {w}x{h} ({:.3} MP)  {matrix:?} / {range:?} / {siting:?}", (w as f64 * h as f64) / 1e6);
    println!("  {} bytes RGB8 (stride {})  in {ms:.2} ms", rgb.len(), w * 3);
    println!("  sha256  {}", sha256_hex(&rgb));

    if let Some(dump) = arg(&args, "--dump") {
        std::fs::write(&dump, &rgb).expect("write the dump");
        println!("  dumped  {dump}  (verify with: Get-FileHash -Algorithm SHA256 {dump})");
    }

    // ── the WIC delta ──
    let Some(heic) = arg(&args, "--wic") else { return };
    let at = arg(&args, "--at").unwrap_or_else(|| "0,0".to_string());
    let (xs, ys) = at.split_once(',').expect("--at must look like 3584,2048");
    let (ox, oy) = (xs.parse::<u32>().unwrap(), ys.parse::<u32>().unwrap());

    let heic = PathBuf::from(&heic);
    let dir = heic.parent().unwrap_or(Path::new("."));
    let stem = heic.file_stem().unwrap().to_string_lossy().to_string();
    let shots = falcon_decode::scan_folder(dir).expect("scan the folder");
    let shot = shots
        .iter()
        .find(|s| s.jpg.as_ref().map(|p| p.file_stem().unwrap().to_string_lossy() == stem).unwrap_or(false))
        .unwrap_or_else(|| panic!("no shot for {stem} in {}", dir.display()));

    println!("\nWIC     : decoding {} in full (this is the ~1 s pure-CPU path)…", heic.display());
    let t = std::time::Instant::now();
    let (wic, ww, wh) = falcon_decode::decode_full_rgb(shot).expect("WIC decode");
    println!("  {ww}x{wh} in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    assert!(ox + w <= ww && oy + h <= wh, "region {ox},{oy} {w}x{h} is outside the {ww}x{wh} decode");

    // Per-channel delta over the region.
    let mut max = [0i32; 3];
    let mut sum = [0i64; 3];
    let mut hist = [0u64; 3]; // count of |delta| > 0, > 2, > 8
    let mut worst = (0i32, 0u32, 0u32, 0usize);
    let n = (w as u64) * (h as u64);
    for row in 0..h {
        for col in 0..w {
            let a = ((row * w + col) * 3) as usize;
            let b = (((oy + row) * ww + (ox + col)) * 3) as usize;
            for c in 0..3 {
                let d = rgb[a + c] as i32 - wic[b + c] as i32;
                let ad = d.abs();
                sum[c] += ad as i64;
                if ad > max[c] {
                    max[c] = ad;
                }
                if ad > 0 {
                    hist[0] += 1;
                }
                if ad > 2 {
                    hist[1] += 1;
                }
                if ad > 8 {
                    hist[2] += 1;
                }
                if ad > worst.0 {
                    worst = (ad, col, row, c);
                }
            }
        }
    }
    println!("\nDELTA vs the shipping WIC path, region ({ox},{oy}) {w}x{h}:");
    for (c, nm) in ["R", "G", "B"].iter().enumerate() {
        println!("  {nm}: max {:>3}   mean {:.4}", max[c], sum[c] as f64 / n as f64);
    }
    let total = n * 3;
    println!(
        "  samples differing at all: {} / {total} ({:.2}%);  by >2: {:.2}%;  by >8: {:.3}%",
        hist[0],
        100.0 * hist[0] as f64 / total as f64,
        100.0 * hist[1] as f64 / total as f64,
        100.0 * hist[2] as f64 / total as f64
    );
    println!("  worst sample: |{}| at ({}, {}) channel {}", worst.0, worst.1, worst.2, ["R", "G", "B"][worst.3]);

    // ── the parameter sweep ──
    // The single delta above says "close". It does NOT say the parameters were read correctly —
    // and reading them correctly is the whole claim of this stage. So run every (matrix, range,
    // siting) the kernel models against the SAME WIC decode. If the VUI read is right, the file's
    // own declared combination must win by a wide margin; a wrong matrix or a wrong range is a
    // systematic bias no chroma filter could ever account for, and it shows up here as an order of
    // magnitude, not as a tie.
    println!("\nPARAMETER SWEEP against the same WIC decode (the VUI read, cross-examined):");
    println!("  {:<26} {:>8} {:>8} {:>8} {:>10}", "params", "max R", "max G", "max B", "mean |d|");
    let mut best: Option<(f64, String)> = None;
    for m in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
        for r in [YuvRange::Full, YuvRange::Limited] {
            for s in [ChromaSiting::Left, ChromaSiting::Center] {
                let alt = nv12_to_rgb8(&f, YuvParams { matrix: m, range: r, siting: s }).expect("convert");
                let mut mx = [0i32; 3];
                let mut sm = 0i64;
                for row in 0..h {
                    for col in 0..w {
                        let a = ((row * w + col) * 3) as usize;
                        let b = (((oy + row) * ww + (ox + col)) * 3) as usize;
                        for c in 0..3 {
                            let d = (alt[a + c] as i32 - wic[b + c] as i32).abs();
                            sm += d as i64;
                            if d > mx[c] {
                                mx[c] = d;
                            }
                        }
                    }
                }
                let mean = sm as f64 / (n * 3) as f64;
                let label = format!("{m:?}/{r:?}/{s:?}");
                println!("  {label:<26} {:>8} {:>8} {:>8} {mean:>10.4}", mx[0], mx[1], mx[2]);
                if best.as_ref().map(|(bm, _)| mean < *bm).unwrap_or(true) {
                    best = Some((mean, label));
                }
            }
        }
    }
    let (bm, bl) = best.unwrap();
    println!("  -> closest to WIC: {bl} (mean |d| {bm:.4})");
}
