//! v0.8.146 (E3-M1) — the D3D11VA decode's DIAGNOSTICS HOOK. An example binary, so nothing links
//! it into the app, and nothing in `cargo test` depends on it.
//!
//! Three jobs the suite deliberately does not do, because each wants a whole real photo, a quiet
//! GPU, or both:
//!
//!   1. **Timing.** Per-tile decode through the marshalling, one session reused across all 54
//!      tiles, and the CPU cost while it runs. Stage 0 could not obtain a real D3D11VA decode time
//!      at all — this is that missing Windows number.
//!   2. **The Qmatrix falsifier at scale.** Every tile of a file decoded three ways
//!      (`FromParameterSets` / `Zeroed` / `Omitted`), reporting how many differ.
//!   3. **The hostile run.** N byte-flip trials against a real tile, reporting survival and
//!      whether the device stayed usable.
//!
//! ```text
//! cargo run -p falcon-hwdec --example hwdec_probe --release -- <file.heic> [--tiles N]
//!     [--reps R] [--falsify] [--hostile N] [--dump-tile K <out.nv12>]
//! ```

use std::path::PathBuf;
use std::time::Instant;

#[cfg(not(windows))]
fn main() {
    println!("falcon-hwdec is a Windows/D3D11VA stage; this probe does nothing elsewhere.");
}

#[cfg(windows)]
fn main() {
    use falcon_decode::yuv_kernel::sha256_hex;
    use falcon_hwdec::dxva::QmatrixPolicy;
    use falcon_hwdec::{tile_source, DecodeSession};

    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    let flag = |name: &str| args.iter().any(|a| a == name);

    let Some(path) = args.get(1).filter(|a| !a.starts_with("--")).map(PathBuf::from) else {
        eprintln!("usage: hwdec_probe <file.heic> [--tiles N] [--reps R] [--falsify] [--hostile N]");
        std::process::exit(2);
    };

    let src = match tile_source(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("container: {e}");
            std::process::exit(1);
        }
    };
    let sps = &src.params.sps;
    println!("file    : {}", path.display());
    println!(
        "grid    : {}x{} = {} tiles of {}x{}   profile_idc {} level {}   {}-bit 4:{}:{}",
        src.cols,
        src.rows,
        src.tiles.len(),
        src.tile_w,
        src.tile_h,
        sps.profile_idc,
        sps.level_idc as f32 / 30.0,
        sps.bit_depth_luma_minus8 + 8,
        if sps.chroma_format_idc == 1 { 2 } else { 4 },
        if sps.chroma_format_idc == 1 { 0 } else { 4 },
    );
    println!(
        "scaling : enabled={} sps_data_present={} explicit_matrices={} (pps_data_present={})",
        sps.scaling_list_enabled_flag,
        sps.sps_scaling_list_data_present_flag,
        src.params.effective_scaling_lists().explicit_matrices,
        src.params.pps.pps_scaling_list_data_present_flag,
    );
    println!(
        "vui     : full_range={} matrix_coeffs={} primaries={} transfer={} chroma_loc={:?}  sps_tail_verified={} pps_tail_verified={}",
        sps.vui.video_full_range_flag,
        sps.vui.matrix_coeffs,
        sps.vui.colour_primaries,
        sps.vui.transfer_characteristics,
        if sps.vui.chroma_loc_info_present {
            Some(sps.vui.chroma_sample_loc_type_top_field)
        } else {
            None
        },
        sps.tail_verified,
        src.params.pps.tail_verified,
    );

    let want = arg("--tiles").and_then(|s| s.parse::<usize>().ok()).unwrap_or(src.tiles.len());
    let n = want.min(src.tiles.len());
    let reps = arg("--reps").and_then(|s| s.parse::<usize>().ok()).unwrap_or(9);

    let mut session = match DecodeSession::new(src.tile_w, src.tile_h, 8) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("no decode session: {e}");
            std::process::exit(1);
        }
    };
    let (raw, specific) = session.config();
    println!(
        "session : {}x{} surfaces={} ConfigBitstreamRaw={} ConfigDecoderSpecific=0x{:04X}\n",
        session.width(),
        session.height(),
        session.surface_count(),
        raw,
        specific
    );

    if let (Some(k), Some(out)) = (
        arg("--dump-tile").and_then(|s| s.parse::<usize>().ok()),
        args.iter().position(|a| a == "--dump-tile").and_then(|i| args.get(i + 2)).cloned(),
    ) {
        match session.decode_tile(&src.params, &src.tiles[k]) {
            Ok(img) => {
                let packed = img.packed();
                std::fs::write(&out, &packed).expect("write the dump");
                println!(
                    "dumped tile {k}: {} bytes stride={} -> {out}\n  sha256 {}",
                    packed.len(),
                    img.stride,
                    sha256_hex(&packed)
                );
            }
            Err(e) => eprintln!("tile {k}: {e}"),
        }
        return;
    }

    // ── correctness first: one pass, digests per tile ──
    println!("{:<6} {:>10} {:>8}  sha256(packed NV12)", "tile", "payload B", "stride");
    for k in 0..n.min(4) {
        match session.decode_tile(&src.params, &src.tiles[k]) {
            Ok(img) => println!(
                "{:<6} {:>10} {:>8}  {}",
                k,
                src.tiles[k].len(),
                img.stride,
                sha256_hex(&img.packed())
            ),
            Err(e) => println!("{k:<6} {:>10} {:>8}  ERR {e}", src.tiles[k].len(), "-"),
        }
    }

    // ── timing: the whole photo through ONE reused session ──
    let items: Vec<Vec<u8>> = src.tiles[..n].to_vec();
    println!("\nTIMING — {n} tiles, one reused session, median of {reps} reps");

    let mut totals = Vec::new();
    let mut split = (0.0f64, 0.0f64);
    let cpu0 = cpu_time_ms();
    let wall0 = Instant::now();
    for _ in 0..reps {
        let t = Instant::now();
        let (mut sub, mut rb) = (0.0f64, 0.0f64);
        for it in &items {
            if let Err(e) = session.decode_tile(&src.params, it) {
                eprintln!("  {e}");
                break;
            }
            let (a, b) = session.last_split_ms();
            sub += a;
            rb += b;
        }
        totals.push(t.elapsed().as_secs_f64() * 1e3);
        split = (sub, rb);
    }
    let wall = wall0.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_time_ms() - cpu0;
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = totals[totals.len() / 2];
    println!(
        "  serialised (Map per tile): median {med:.2} ms / {n} tiles = {:.3} ms/tile   (min {:.2}, max {:.2})",
        med / n as f64,
        totals[0],
        totals[totals.len() - 1]
    );
    println!(
        "    of which submit(CPU marshalling) {:.2} ms and readback(GPU wait + 1.3 MB copy) {:.2} ms",
        split.0, split.1
    );
    println!(
        "  process CPU over the whole timed block: {cpu:.1} ms of {wall:.1} ms wall = {:.0}% of one core",
        100.0 * cpu / wall
    );

    let mut ptot = Vec::new();
    let pcpu0 = cpu_time_ms();
    let pwall0 = Instant::now();
    for _ in 0..reps {
        let t = Instant::now();
        match session.decode_tiles(&src.params, &items) {
            Ok(v) => assert_eq!(v.len(), items.len()),
            Err(e) => {
                eprintln!("  pipelined: {e}");
                break;
            }
        }
        ptot.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let pwall = pwall0.elapsed().as_secs_f64() * 1e3;
    let pcpu = cpu_time_ms() - pcpu0;
    if !ptot.is_empty() {
        ptot.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pmed = ptot[ptot.len() / 2];
        println!(
            "  pipelined ({} in flight): median {pmed:.2} ms / {n} tiles = {:.3} ms/tile   (min {:.2}, max {:.2})",
            session.surface_count(),
            pmed / n as f64,
            ptot[0],
            ptot[ptot.len() - 1]
        );
        println!(
            "  process CPU over the pipelined block: {pcpu:.1} ms of {pwall:.1} ms wall = {:.0}% of one core",
            100.0 * pcpu / pwall
        );
    }

    // ── the Qmatrix falsifier ──
    if flag("--falsify") {
        // Each policy gets a FRESH session. Sharing one would let the previous submission's
        // matrices latch in driver state, which is exactly what made an earlier draft of this
        // measurement read 4/8 instead of a clean answer.
        println!("\nQMATRIX FALSIFIER — {n} tiles, each policy on its own session");
        let digest_for = |policy: QmatrixPolicy| -> Vec<String> {
            let mut s = DecodeSession::new(src.tile_w, src.tile_h, 8).expect("a session");
            (0..n)
                .map(|k| match s.decode_tile_with(&src.params, &src.tiles[k], policy) {
                    Ok(i) => sha256_hex(&i.packed()),
                    Err(e) => format!("ERR {e}"),
                })
                .collect()
        };
        let good = digest_for(QmatrixPolicy::FromParameterSets);
        for (label, policy) in
            [("zeroed", QmatrixPolicy::Zeroed), ("omitted", QmatrixPolicy::Omitted)]
        {
            let alt = digest_for(policy);
            let diff = good.iter().zip(&alt).filter(|(a, b)| a != b).count();
            let errs = alt.iter().filter(|d| d.starts_with("ERR")).count();
            println!("  {label:<8} matrices: {diff}/{n} tiles differ ({errs} declined outright)");
        }
    }

    // ── the hostile run ──
    if let Some(trials) = arg("--hostile").and_then(|s| s.parse::<usize>().ok()) {
        println!("\nHOSTILE — {trials} byte-flip trials against tile 0's payload");
        let base = src.tiles[0].clone();
        let mut ok = 0usize;
        let mut err = 0usize;
        let mut state = 0x2026_0805u64;
        for _ in 0..trials {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let at = (state >> 33) as usize % base.len();
            let bit = ((state >> 17) & 7) as u8;
            let mut m = base.clone();
            m[at] ^= 1 << bit;
            match session.decode_tile(&src.params, &m) {
                Ok(_) => ok += 1,
                Err(_) => err += 1,
            }
        }
        println!("  survived all {trials}: decoded {ok}, declined {err}, panicked 0");
        match session.decode_tile(&src.params, &src.tiles[0]) {
            Ok(img) => println!(
                "  the device is still usable afterwards: tile 0 sha256 {}",
                sha256_hex(&img.packed())
            ),
            Err(e) => println!("  DEVICE UNUSABLE AFTERWARDS: {e}"),
        }
    }
}

/// Process CPU time (user + kernel) in milliseconds.
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
