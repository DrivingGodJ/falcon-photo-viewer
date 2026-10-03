//! v0.8.147 (E3-M2) — the full-photo assembly's DIAGNOSTICS HOOK. An example binary, so nothing
//! links it into the app and nothing in `cargo test` depends on it.
//!
//! Four jobs the suite deliberately does not do, because each wants whole real photos, a quiet GPU,
//! or both:
//!
//!   1. **The geometry table.** What E1 says each corpus file is: grid, mosaic, crop, `irot`, VUI.
//!   2. **The dims table.** This path's answer vs the shipping WIC path's, per file per tier.
//!   3. **The pixel report.** Delta distribution against WIC at native and at a tier, plus the
//!      SEAM HUNT — the same deltas restricted to the tile-boundary columns and rows, which is
//!      where a compositor's edge arithmetic goes wrong and where a whole-image mean hides it.
//!   4. **Timing.** Photo → final RGB at each tier, against the WIC baseline on the same box, with
//!      the decode/assemble split and the process CPU cost.
//!
//! ```text
//! cargo run -p falcon-hwdec --example photo_probe --release -- [dir-or-file ...] [--tier N]
//!     [--reps R] [--no-baseline] [--falsify]
//! ```

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

#[cfg(not(windows))]
fn main() {
    println!("falcon-hwdec is a Windows/D3D11VA stage; this probe does nothing elsewhere.");
}

#[cfg(windows)]
fn main() {
    windows_main();
}

#[cfg(windows)]
fn windows_main() {
    use std::path::PathBuf;
    use std::time::Instant;

    use falcon_decode::{browse_frame_rgba, scan_folder, Lane, Shot};
    use falcon_hwdec::{tile_source, AssemblyFault, PhotoDecoder, Submission, TileSource};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let reps = arg("--reps").and_then(|s| s.parse::<usize>().ok()).unwrap_or(3);
    let tier = arg("--tier").and_then(|s| s.parse::<u32>().ok()).unwrap_or(2880);
    let baseline = !flag("--no-baseline");

    // Inputs: whatever paths were named, else the two corpus directories.
    let mut named: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect();
    if named.is_empty() {
        named = vec![
            fixture_paths::heic().to_path_buf(),
            fixture_paths::photos().to_path_buf(),
        ];
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for p in named {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    let q = e.path();
                    if q.extension().is_some_and(|x| x.eq_ignore_ascii_case("heic")) {
                        files.push(q);
                    }
                }
            }
        } else {
            files.push(p);
        }
    }
    files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
    files.dedup_by_key(|p| p.file_name().map(|n| n.to_owned()));
    if files.is_empty() {
        eprintln!("no HEIC files found");
        std::process::exit(2);
    }

    // The shipping path's shots, by file name, so the baseline is the REAL lane and not a
    // re-implementation of it.
    let mut shots: Vec<Shot> = Vec::new();
    for d in [
        fixture_paths::heic().to_path_buf(),
        fixture_paths::photos().to_path_buf(),
    ] {
        if d.is_dir() {
            if let Ok(found) = scan_folder(&d) {
                shots.extend(found.into_iter().filter(|s| {
                    s.jpg.as_ref().is_some_and(|p| {
                        p.extension().is_some_and(|x| x.eq_ignore_ascii_case("heic"))
                    })
                }));
            }
        }
    }
    let shot_for = |path: &std::path::Path| -> Option<Shot> {
        let name = path.file_name()?;
        shots.iter().find(|s| s.jpg.as_deref().and_then(|p| p.file_name()) == Some(name)).cloned()
    };

    // ── 1. geometry ──
    println!("=== GEOMETRY (E1) ===");
    println!(
        "{:<22} {:>9} {:>7} {:>11} {:>16} {:>11} {:>5} {:>7}  vui",
        "file", "grid", "tiles", "tile", "mosaic", "crop", "irot", "display"
    );
    let mut sources: Vec<(PathBuf, TileSource)> = Vec::new();
    for f in &files {
        match tile_source(f) {
            Ok(s) => {
                let v = &s.params.sps.vui;
                println!(
                    "{:<22} {:>4}x{:<4} {:>7} {:>5}x{:<5} {:>7}x{:<8} {:>5}x{:<5} {:>5} {:>4}x{:<4}  mtx={} full={} loc={:?} pri={} trc={}",
                    f.file_name().unwrap().to_string_lossy(),
                    s.cols,
                    s.rows,
                    s.tiles.len(),
                    s.tile_w,
                    s.tile_h,
                    s.mosaic.0,
                    s.mosaic.1,
                    s.crop.w,
                    s.crop.h,
                    s.irot,
                    s.display.0,
                    s.display.1,
                    v.matrix_coeffs,
                    v.video_full_range_flag,
                    v.chroma_loc_info_present.then_some(v.chroma_sample_loc_type_top_field),
                    v.colour_primaries,
                    v.transfer_characteristics,
                );
                sources.push((f.clone(), s));
            }
            Err(e) => println!("{:<22} CONTAINER: {e}", f.file_name().unwrap().to_string_lossy()),
        }
    }

    // The tiers the shipping path actually asks for: `fast_decode_target(max_dim, supersample)`.
    let tiers: Vec<(&str, Option<u32>)> = vec![
        ("native", None),
        ("detail 8192", Some(8192)),
        ("fast sup 2880", Some(2880)),
        ("fast sub 1440", Some(1440)),
        ("thumb 256", Some(256)),
    ];

    // ── 2. dims ──
    println!("\n=== DIMS GATE — hw assembly vs the shipping WIC path ===");
    println!("{:<22} {:<14} {:>12} {:>12}  {}", "file", "tier", "hw", "wic", "verdict");
    for (path, src) in &sources {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(shot) = shot_for(path) else {
            println!("{name:<22} (no shot — the shipping scanner did not classify this file)");
            continue;
        };
        let mut dec = match PhotoDecoder::new(src, 8) {
            Ok(d) => d,
            Err(e) => {
                println!("{name:<22} DECODER: {e}");
                continue;
            }
        };
        for (label, scale) in &tiers {
            let hw = match dec.decode(src, *scale) {
                Ok(p) => (p.w, p.h),
                Err(e) => {
                    println!("{name:<22} {label:<14} HW ERR {e}");
                    continue;
                }
            };
            let ask = scale.unwrap_or(16384);
            let wic = match browse_frame_rgba(&shot, ask, true, Lane::Native) {
                Ok(f) => (f.w, f.h),
                Err(e) => {
                    println!("{name:<22} {label:<14} WIC ERR {e}");
                    continue;
                }
            };
            println!(
                "{name:<22} {label:<14} {:>5}x{:<6} {:>5}x{:<6}  {}",
                hw.0,
                hw.1,
                wic.0,
                wic.1,
                if hw == wic { "MATCH" } else { "*** MISMATCH ***" }
            );
        }
    }

    // ── 3. pixels + seam hunt ──
    println!("\n=== PIXEL REPORT vs the shipping WIC path ===");
    for (path, src) in &sources {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(shot) = shot_for(path) else { continue };
        let Ok(mut dec) = PhotoDecoder::new(src, 8) else { continue };
        for (label, scale) in [("native", None), ("tier", Some(tier))] {
            let Ok(hw) = dec.decode(src, scale) else { continue };
            let ask = scale.unwrap_or(16384);
            let Ok(w) = browse_frame_rgba(&shot, ask, true, Lane::Native) else { continue };
            if (w.w, w.h) != (hw.w, hw.h) {
                println!("{name:<22} {label:<7} dims disagree — no pixel comparison");
                continue;
            }
            let wic = rgba_to_rgb(&w.rgba);
            print!("{name:<22} {label:<7} ");
            print_delta(&hw.rgb, &wic, hw.w, hw.h);

            if scale.is_none() {
                // THE SEAM HUNT. Every internal tile boundary in DISPLAY space, ±2 px, against the
                // same-sized rest of the picture. A compositor that places a tile wrong shows here
                // and nowhere else — a whole-image mean over 48 Mpx would bury 8 bad columns.
                let seam = seam_columns(src);
                let (edge, body) = split_by_columns(&hw.rgb, &wic, hw.w, hw.h, &seam);
                println!(
                    "{:<22} {:<7}   seam hunt: {} boundary bands — edge max {} mean {:.4}  |  body max {} mean {:.4}",
                    "", "", seam.len(), edge.0, edge.1, body.0, body.1
                );
                // The chroma PHASE split, which is what tells a reader whether the residual is a
                // reconstruction difference or a defect. With LEFT siting an even luma column sits
                // exactly on a chroma sample and an odd one is interpolated; both luma ROWS are
                // interpolated (4:2:0 is vertically interstitial in either siting). So a residual
                // that is a chroma-filter disagreement is LOWEST at (even x, even y) and rises with
                // the interpolation weight, while a geometry or colour defect is phase-blind.
                let ph = chroma_phase(&hw.rgb, &wic, hw.w, hw.h);
                println!(
                    "{:<22} {:<7}   chroma phase mean |d|: (x even,y even) {:.4}  (odd,even) {:.4}  (even,odd) {:.4}  (odd,odd) {:.4}",
                    "", "", ph[0], ph[1], ph[2], ph[3]
                );
            }
        }
        // The resampler's own contribution, isolated: the SAME hw native pixels reduced by the
        // shipping CPU Lanczos vs by the GPU one. Whatever is left in the tier row above this is
        // the assembly's, not the filter's.
        if let (Ok(nat), Ok(t)) = (dec.decode(src, None), dec.decode(src, Some(tier))) {
            let (rgba, cw, ch) =
                falcon_decode::downscale_rgba(&falcon_decode::rgb_to_rgba(&nat.rgb), nat.w, nat.h, tier);
            if (cw, ch) == (t.w, t.h) {
                print!("{:<22} {:<7} ", "", "resamp");
                print_delta(&t.rgb, &rgba_to_rgb(&rgba), t.w, t.h);
            }
        }
    }

    // ── 4. timing ──
    println!("\n=== TIMING — photo to final RGB, median of {reps} ===");
    println!(
        "{:<22} {:<14} {:>10} {:>10} {:>10} {:>10}  {}",
        "file", "tier", "hw ms", "dec+up", "asm+read", "wic ms", "speedup"
    );
    for (path, src) in &sources {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let shot = shot_for(path);
        let Ok(mut dec) = PhotoDecoder::new(src, 8) else { continue };
        for (label, scale) in &tiers {
            let mut ms = Vec::new();
            let mut split = (0.0f64, 0.0f64);
            let cpu0 = cpu_time_ms();
            let wall0 = Instant::now();
            for _ in 0..reps {
                let t = Instant::now();
                if dec.decode(src, *scale).is_err() {
                    break;
                }
                ms.push(t.elapsed().as_secs_f64() * 1e3);
                split = dec.last_split_ms();
            }
            if ms.is_empty() {
                continue;
            }
            let cpu_pct = 100.0 * (cpu_time_ms() - cpu0) / wall0.elapsed().as_secs_f64().max(1e-9) / 1e3;
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let hw = ms[ms.len() / 2];
            let wic = if baseline {
                shot.as_ref().and_then(|s| {
                    let ask = scale.unwrap_or(16384);
                    let mut v = Vec::new();
                    for _ in 0..reps {
                        let t = Instant::now();
                        browse_frame_rgba(s, ask, true, Lane::Native).ok()?;
                        v.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    Some(v[v.len() / 2])
                })
            } else {
                None
            };
            println!(
                "{name:<22} {label:<14} {hw:>10.1} {:>10.1} {:>10.1} {:>10}  {}   cpu {:.0}% of one core",
                split.0,
                split.1,
                wic.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into()),
                wic.map(|v| format!("{:.1}x", v / hw)).unwrap_or_else(|| "-".into()),
                cpu_pct,
            );
        }
    }

    // ── the falsifiers, at photo scale ──
    if flag("--falsify") {
        println!("\n=== FALSIFIERS ===");
        for (path, src) in &sources {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Ok(mut dec) = PhotoDecoder::new(src, 8) else { continue };
            let Ok(good) = dec.decode(src, None) else { continue };
            for (label, fault) in [
                ("tile offset", AssemblyFault::TileOffset),
                ("drop irot", AssemblyFault::DropRotation),
                ("reverse irot", AssemblyFault::ReverseRotation),
                ("scale -1", AssemblyFault::ScaleOffByOne),
            ] {
                match dec.decode_with(src, None, Submission::Pipelined, fault) {
                    Ok(bad) => {
                        if (bad.w, bad.h) != (good.w, good.h) {
                            println!(
                                "{name:<22} {label:<12} dims moved {}x{} -> {}x{}",
                                good.w, good.h, bad.w, bad.h
                            );
                        } else {
                            let seam = seam_columns(src);
                            let (e, b) =
                                split_by_columns(&good.rgb, &bad.rgb, good.w, good.h, &seam);
                            println!(
                                "{name:<22} {label:<12} edge max {} mean {:.4} | body max {} mean {:.4}",
                                e.0, e.1, b.0, b.1
                            );
                        }
                    }
                    Err(e) => println!("{name:<22} {label:<12} ERR {e}"),
                }
            }
        }
    }
}

#[cfg(windows)]
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len() / 4 * 3];
    for (d, s) in out.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
        d.copy_from_slice(&s[..3]);
    }
    out
}

/// Per-channel max / mean / share above 8 — the E2-shaped report the milestone asks for.
#[cfg(windows)]
fn print_delta(a: &[u8], b: &[u8], w: u32, h: u32) {
    if a.len() != b.len() {
        println!("length {} vs {}", a.len(), b.len());
        return;
    }
    let mut max = [0u32; 3];
    let mut sum = [0u64; 3];
    let mut over = [0u64; 3];
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        let c = i % 3;
        let d = (*x as i32 - *y as i32).unsigned_abs();
        max[c] = max[c].max(d);
        sum[c] += d as u64;
        if d > 8 {
            over[c] += 1;
        }
    }
    let n = (w as u64) * (h as u64);
    println!(
        "{w}x{h}  R max {:>3} mean {:.4} >8 {:.4}%  |  G max {:>3} mean {:.4} >8 {:.4}%  |  B max {:>3} mean {:.4} >8 {:.4}%",
        max[0], sum[0] as f64 / n as f64, 100.0 * over[0] as f64 / n as f64,
        max[1], sum[1] as f64 / n as f64, 100.0 * over[1] as f64 / n as f64,
        max[2], sum[2] as f64 / n as f64, 100.0 * over[2] as f64 / n as f64,
    );
    // The DISTRIBUTION and the BIAS, because "mean 1.2, max 35" is two numbers that a systematic
    // half-LSB offset and a genuine reconstruction difference produce alike. A signed mean near
    // zero with a long thin tail is a filter disagreement; a signed mean near the unsigned one is
    // an offset, and an offset would be ours to fix.
    let mut hist = [0u64; 7];
    let mut signed = [0i64; 3];
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        signed[i % 3] += *x as i64 - *y as i64;
        let d = (*x as i32 - *y as i32).unsigned_abs();
        let b = match d {
            0 => 0,
            1 => 1,
            2 => 2,
            3..=4 => 3,
            5..=8 => 4,
            9..=16 => 5,
            _ => 6,
        };
        hist[b] += 1;
    }
    // THE DECOMPOSITION that settles what the residual IS. HEVC decode is bit-exact by spec, so the
    // luma planes of the two decodes are the same numbers; if the residual is the two converters
    // reconstructing the half-resolution CHROMA differently, then projecting (dR,dG,dB) back
    // through the forward BT.601 matrix must leave dY at the noise floor and put everything in
    // dCb/dCr. If instead dY were comparable to dCb/dCr, the disagreement would be in the luma
    // path — which would be ours, and a defect.
    let (mut dy, mut dcb, mut dcr) = (0f64, 0f64, 0f64);
    for (x, y) in a.chunks_exact(3).zip(b.chunks_exact(3)) {
        let (r, g, bl) =
            (x[0] as f64 - y[0] as f64, x[1] as f64 - y[1] as f64, x[2] as f64 - y[2] as f64);
        dy += (0.299 * r + 0.587 * g + 0.114 * bl).abs();
        dcb += (-0.168736 * r - 0.331264 * g + 0.5 * bl).abs();
        dcr += (0.5 * r - 0.418688 * g - 0.081312 * bl).abs();
    }
    println!(
        "{:<31}residual decomposed (BT.601 forward): mean |dY| {:.4}   mean |dCb| {:.4}   mean |dCr| {:.4}",
        "",
        dy / n as f64,
        dcb / n as f64,
        dcr / n as f64
    );
    let tot = (a.len() as f64).max(1.0);
    println!(
        "{:<31}|d| histogram: 0 {:.2}%  1 {:.2}%  2 {:.2}%  3-4 {:.3}%  5-8 {:.3}%  9-16 {:.4}%  >16 {:.4}%   signed mean R {:+.4} G {:+.4} B {:+.4}",
        "",
        100.0 * hist[0] as f64 / tot,
        100.0 * hist[1] as f64 / tot,
        100.0 * hist[2] as f64 / tot,
        100.0 * hist[3] as f64 / tot,
        100.0 * hist[4] as f64 / tot,
        100.0 * hist[5] as f64 / tot,
        100.0 * hist[6] as f64 / tot,
        signed[0] as f64 / n as f64,
        signed[1] as f64 / n as f64,
        signed[2] as f64 / n as f64,
    );
}

/// The DISPLAY-space columns that a tile boundary lands on, ±2 px.
///
/// After a 90°/270° rotation the mosaic's vertical seams become horizontal ones, so the bands are
/// derived from the display orientation rather than from the mosaic — a hunt that looked at mosaic
/// columns would find nothing at all on the rotated file, which is the one most likely to be wrong.
#[cfg(windows)]
fn seam_columns(src: &falcon_hwdec::TileSource) -> Vec<u32> {
    let quarter = src.irot % 180 != 0;
    // In display space the seams that run vertically come from the mosaic's tile COLUMNS when the
    // image is upright, and from its tile ROWS when it is turned a quarter.
    let (pitch, count, extent) = if quarter {
        (src.tile_h, src.rows, src.display.0)
    } else {
        (src.tile_w, src.cols, src.display.0)
    };
    let mut out = Vec::new();
    for k in 1..count {
        let x = k * pitch;
        for d in 0..4u32 {
            let c = x.saturating_sub(2) + d;
            if c < extent {
                out.push(c);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Mean |delta| by chroma phase: `[(even x, even y), (odd, even), (even, odd), (odd, odd)]`.
///
/// Computed in MOSAIC orientation, because the chroma lattice belongs to the decoded planes and a
/// quarter turn would scramble the phases; on a rotated file the display axes are swapped, which is
/// exactly what the `quarter` branch undoes.
#[cfg(windows)]
fn chroma_phase(a: &[u8], b: &[u8], w: u32, h: u32) -> [f64; 4] {
    let mut sum = [0u64; 4];
    let mut n = [0u64; 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = (y * w as usize + x) * 3;
            let d = (0..3)
                .map(|c| (a[o + c] as i32 - b[o + c] as i32).unsigned_abs() as u64)
                .max()
                .unwrap_or(0);
            let k = (x & 1) | ((y & 1) << 1);
            sum[k] += d;
            n[k] += 1;
        }
    }
    [
        sum[0] as f64 / n[0].max(1) as f64,
        sum[1] as f64 / n[1].max(1) as f64,
        sum[2] as f64 / n[2].max(1) as f64,
        sum[3] as f64 / n[3].max(1) as f64,
    ]
}

/// `(edge, body)` where each is `(max |delta|, mean |delta|)` — the same statistic computed over
/// the named columns and over everything else.
#[cfg(windows)]
fn split_by_columns(
    a: &[u8],
    b: &[u8],
    w: u32,
    h: u32,
    cols: &[u32],
) -> ((u32, f64), (u32, f64)) {
    let mut mask = vec![false; w as usize];
    for c in cols {
        if (*c as usize) < mask.len() {
            mask[*c as usize] = true;
        }
    }
    let (mut emax, mut esum, mut en) = (0u32, 0u64, 0u64);
    let (mut bmax, mut bsum, mut bn) = (0u32, 0u64, 0u64);
    for y in 0..h as usize {
        let row = y * w as usize * 3;
        for x in 0..w as usize {
            let o = row + x * 3;
            let d = (0..3)
                .map(|c| (a[o + c] as i32 - b[o + c] as i32).unsigned_abs())
                .max()
                .unwrap_or(0);
            if mask[x] {
                emax = emax.max(d);
                esum += d as u64;
                en += 1;
            } else {
                bmax = bmax.max(d);
                bsum += d as u64;
                bn += 1;
            }
        }
    }
    (
        (emax, esum as f64 / en.max(1) as f64),
        (bmax, bsum as f64 / bn.max(1) as f64),
    )
}

#[cfg(windows)]
fn cpu_time_ms() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut c = FILETIME::default();
    let mut e = FILETIME::default();
    let mut k = FILETIME::default();
    let mut u = FILETIME::default();
    // SAFETY: all four out-parameters are live locals; the handle is a pseudo-handle.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok.is_err() {
        return 0.0;
    }
    let to_ms = |f: FILETIME| {
        (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 / 10_000.0
    };
    to_ms(k) + to_ms(u)
}
