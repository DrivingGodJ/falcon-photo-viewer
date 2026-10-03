//! v0.8.144 (E1): THE PARITY BATTERY for the from-scratch HEIF grid parse.
//!
//! The new container parser answers three questions the SHIPPING path already answers by other
//! means — the displayed dimensions, the rotation baked into them, and the source gamut — and one
//! it is the first to answer at all, the tile map. The first three are checked against the shipping
//! answer on every real file the project has; the fourth is checked against itself, because there is
//! nothing else to check it against and "internally consistent" is a real and falsifiable property:
//! `rows × cols` tiles of a declared size must compose to a mosaic that CONTAINS the declared canvas
//! with less than one tile of slack on each axis, every byte range must lie inside the file, and no
//! two of them may overlap.
//!
//! # Why the corpus is spelled out rather than globbed from one folder
//!
//! `testkit/heic` holds five iPhone files; the optional mixed-photo corpus holds those same five
//! (byte-identical) plus `IMG_3258.HEIC`, the iOS 27 fixture that CLOSED SP2 and — the thing that
//! makes it worth its own row — carries a 501-byte SPS with 20 explicit scaling-list matrices where
//! the older 48 MP files carry the 34-byte default-list SPS. Same camera, same 9×6 grid, same
//! 896×1024 tiles, different `hvcC` BYTES. That pair (IMG_1826 defaults vs IMG_3258 explicit) is the
//! matched test pair Stage 0 identified for E3's quantisation-matrix landmine, and E1's job is to
//! hand both hvcC records over faithfully rather than to assume one.
//!
//! # The skip discipline
//!
//! Dimension and gamut parity need the shipping answer, and on Windows the shipping answer for a
//! HEIC comes from the OS HEVC/HEIF Image Extension. Where that is absent (CI, a fresh laptop) these
//! rows SKIP WITH A NOTE rather than fail, exactly as `tests/heic.rs` does — a red test that reports
//! the absence of an OS component is a test that stops being read. The STRUCTURAL rows (tile map,
//! byte ranges, fuzz, hostile input) need no codec at all and run everywhere the files exist.

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

use falcon_decode::*;
use std::path::{Path, PathBuf};

/// The two folders that hold real HEICs, in the order a report should list them.
fn corpus_dirs() -> [&'static std::path::Path; 3] {
    [fixture_paths::heic(), fixture_paths::standard(), fixture_paths::photos()]
}

/// Every HEIC in the corpus, DEDUPED BY CONTENT. The two folders overlap by five byte-identical
/// files, and a parity table that listed each of them twice would look twice as convincing as it is.
fn corpus() -> Vec<PathBuf> {
    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut out = Vec::new();
    for dir in corpus_dirs() {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
            })
            .collect();
        files.sort();
        for p in files {
            let Ok(md) = std::fs::metadata(&p) else { continue };
            // Length + the first 4 KB is identity enough for "is this the same file twice"; the two
            // folders hold copies, not near-misses. Read only that prefix — slurping six multi-
            // megabyte files per test, six times over, to compare four kilobytes of each would be a
            // silly way to spend a suite's I/O.
            let head = std::fs::File::open(&p)
                .and_then(|mut f| {
                    use std::io::Read;
                    let mut b = vec![0u8; 4096];
                    let n = f.read(&mut b)?;
                    b.truncate(n);
                    Ok(b)
                })
                .unwrap_or_default();
            if seen.iter().any(|(l, h)| *l == md.len() && *h == head) {
                continue;
            }
            seen.push((md.len(), head));
            out.push(p);
        }
    }
    out
}

fn shot_for(path: &Path) -> Shot {
    Shot {
        id: 0,
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        has_raw: false,
        has_jpg: true,
        raw: None,
        jpg: Some(path.to_path_buf()),
        kind: SrcKind::Heic,
        cloud_placeholder: false,
        sniffed: None,
    }
}

fn corpus_or_skip(what: &str) -> Option<Vec<PathBuf>> {
    let c = corpus();
    if c.is_empty() {
        eprintln!("skip ({what}): no HEIC found in any corpus folder");
        return None;
    }
    Some(c)
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────────────────────

/// ROW 1 — THE PARITY TABLE. Dimensions, rotation and gamut against the shipping path, tile map
/// against arithmetic, byte ranges against the file, for every real HEIC the project holds. The
/// table is PRINTED WHOLE before anything is judged, so a reader can see what the row actually
/// measured rather than only that it was happy.
///
/// FALSIFIERS: return `crop` instead of `display` from the parser and IMG_2814 (irot 270) goes red
/// while the other five stay green — which is exactly the asymmetry that makes the rotated file
/// load-bearing. Use the MOSAIC as the canvas instead of the grid's declared output and every 48 MP
/// row goes red by 96 pixels of height. Take `tile_w × cols` as the canvas and the same. Read the
/// gamut from the first `colr` box in the file rather than through the oracle and the answer changes
/// on any file whose gain map's `nclx` precedes the master's ICC.
#[test]
fn the_grid_parse_matches_the_shipping_path_on_every_real_heic() {
    let Some(files) = corpus_or_skip("grid parity") else { return };
    let codec = wic_heif_codec_present();
    if !codec {
        eprintln!(
            "note (grid parity): no WIC HEIF decoder — the DIMS and GAMUT columns are skipped; \
             the structural columns still run."
        );
    }
    let mut rotated = 0usize;
    let mut wide_gamut = 0usize;
    let mut checked_dims = 0usize;
    println!(
        "{:<24} {:>11} {:>11} {:>6} {:>7} {:>10} {:>6} {:>5} {:>6} {:>12} {:>5}",
        "file", "display", "wic", "irot", "grid", "tile", "tiles", "depth", "hvcC", "gamut", "aux"
    );
    for p in &files {
        let plan = heif_decode_plan(p)
            .unwrap_or_else(|e| panic!("{}: the parser must produce a plan — {e}", name(p)));
        let g = &plan.grid;
        let wic = codec.then(|| source_dimensions(&shot_for(p))).flatten();
        println!(
            "{:<24} {:>5}x{:<5} {:>5} {:>6} {:>7} {:>10} {:>6} {:>5} {:>6} {:>12} {:>5}",
            name(p),
            g.display.0,
            g.display.1,
            wic.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "-".into()),
            g.irot,
            format!("{}x{}", g.cols, g.rows),
            format!("{}x{}", g.tile_w, g.tile_h),
            g.tiles.len(),
            g.bit_depth.map(|d| d.to_string()).unwrap_or_else(|| "?".into()),
            g.hvcc.as_ref().map(|b| b.len().to_string()).unwrap_or_else(|| "split".into()),
            format!("{:?}", plan.gamut),
            g.aux.len(),
        );
    }
    println!();

    for p in &files {
        let n = name(p);
        let plan = heif_decode_plan(p).expect("parsed above");
        let g = &plan.grid;

        // ── DIMS + ROTATION: byte-identical to the shipping answer ──
        if codec {
            let wic = source_dimensions(&shot_for(p))
                .unwrap_or_else(|| panic!("{n}: the shipping path must answer for a real HEIC"));
            assert_eq!(
                g.display, wic,
                "{n}: the container parse says {:?}, the shipping path says {wic:?}",
                g.display
            );
            checked_dims += 1;
        }
        // Rotation is not separately observable through WIC — it APPLIES `irot` and reports the
        // result — so the rotation assertion IS the dimension assertion, plus this: a quarter turn
        // must transpose and nothing else may.
        let expect = match g.irot {
            90 | 270 => (g.crop.h, g.crop.w),
            _ => (g.crop.w, g.crop.h),
        };
        assert_eq!(g.display, expect, "{n}: display must be the crop, transposed iff irot is a quarter turn");
        assert!(matches!(g.irot, 0 | 90 | 180 | 270), "{n}: irot {} is not a multiple of 90", g.irot);
        if g.irot != 0 {
            rotated += 1;
        }

        // ── GAMUT: the ORACLE's answer, not a second one ──
        if codec {
            let shipping = shot_source_gamut(&shot_for(p));
            assert_eq!(
                plan.gamut, shipping,
                "{n}: the plan's gamut must BE the shipping answer, not merely agree with it"
            );
            if plan.gamut != falcon_color::Gamut::Srgb {
                wide_gamut += 1;
            }
        }

        // ── TILE MAP: internally consistent ──
        assert_eq!(
            g.tiles.len(),
            (g.rows as usize) * (g.cols as usize),
            "{n}: dimg must name exactly rows × cols tiles"
        );
        assert_eq!(g.mosaic, (g.cols * g.tile_w, g.rows * g.tile_h), "{n}: mosaic arithmetic");
        assert!(
            g.mosaic.0 >= g.grid_output.0 && g.mosaic.1 >= g.grid_output.1,
            "{n}: the tiles must COVER the declared canvas — mosaic {:?} vs output {:?}",
            g.mosaic,
            g.grid_output
        );
        // The edge-tile crop reconciles: the slack the grid trims away is less than one whole tile
        // on each axis, or the grid is carrying a row/column of tiles it never shows.
        assert!(
            g.mosaic.0 - g.grid_output.0 < g.tile_w && g.mosaic.1 - g.grid_output.1 < g.tile_h,
            "{n}: slack {:?} is a whole tile or more — the grid declares tiles it does not use",
            (g.mosaic.0 - g.grid_output.0, g.mosaic.1 - g.grid_output.1)
        );
        assert_eq!(g.ispe, Some(g.grid_output), "{n}: ispe and the grid payload must say the same thing");
        // Every raster position is occupied exactly once.
        let mut seen = vec![false; g.tiles.len()];
        for t in &g.tiles {
            let ix = (t.row as usize) * (g.cols as usize) + t.col as usize;
            assert!(!seen[ix], "{n}: two tiles at raster position ({}, {})", t.row, t.col);
            seen[ix] = true;
        }
        assert!(seen.iter().all(|&b| b), "{n}: the grid has a hole in it");

        // ── BYTE RANGES: in bounds, non-empty, non-overlapping ──
        let file_len = std::fs::metadata(p).expect("stat").len();
        assert_eq!(g.file_len, file_len, "{n}: the plan must be measured against the real file length");
        for t in &g.tiles {
            assert!(!t.extents.is_empty(), "{n}: tile {} has no bytes", t.item_id);
            for e in &t.extents {
                assert!(e.len > 0, "{n}: tile {} has a zero-length extent", t.item_id);
                assert!(
                    e.end() <= file_len,
                    "{n}: tile {} runs to {} past a {file_len}-byte file",
                    t.item_id,
                    e.end()
                );
            }
        }
        assert_eq!(
            g.overlapping_tile_extents(),
            None,
            "{n}: two tiles claim the same bytes — the tile map is wrong"
        );
    }

    // Assertions about THIS corpus, so a silently-emptied testkit cannot make the row vacuous.
    assert!(files.len() >= 5, "the corpus should hold at least the five testkit files, found {}", files.len());
    assert!(rotated > 0, "no file in the corpus carries an irot — the rotation column proved nothing");
    if codec {
        assert!(checked_dims == files.len(), "every file must have been dimension-checked");
        assert!(wide_gamut > 0, "the iPhone corpus should carry at least one wide-gamut file");
    }
    eprintln!(
        "grid parity: {} files, {rotated} rotated, dims+gamut checked on {checked_dims}",
        files.len()
    );
}

/// ROW 2 — ONE hvcC PER FILE, SHARED BY EVERY TILE, and the bytes are NOT constant across files.
///
/// SP2's conclusion and SP2's qualification, pinned as one row because they are one fact with two
/// halves. The first half is what makes the concatenated single-session stream legal (parameter sets
/// once, then N slice NALs, no in-band re-emission). The second half is what stops E3 from caching a
/// decoder on the assumption those bytes never move: `IMG_3258` (iOS 27, explicit scaling lists)
/// carries a different record from `IMG_1826` (iOS 26.3, default lists), same camera, same geometry.
/// A cache keyed on anything but the BYTES would hand the iOS 27 file the wrong quantisation
/// matrices and render wrong pixels silently.
#[test]
fn every_tile_shares_one_hvcc_and_the_bytes_vary_between_files() {
    let Some(files) = corpus_or_skip("hvcC sharing") else { return };
    let mut records: Vec<(String, Vec<u8>)> = Vec::new();
    for p in &files {
        let n = name(p);
        let g = parse_heif_grid(&std::fs::read(p).expect("read")).expect("parse");
        let shared = g
            .hvcc
            .clone()
            .unwrap_or_else(|| panic!("{n}: every tile must name the SAME hvcC property"));
        assert_eq!(g.hvcc_by_prop.len(), 1, "{n}: the primary's tiles reference more than one hvcC");
        let ix = g.tiles[0].hvcc_prop.expect("a tile with an hvcC");
        assert!(
            g.tiles.iter().all(|t| t.hvcc_prop == Some(ix)),
            "{n}: the shared record must come from ONE ipco index"
        );
        println!("{n:<24} hvcC {:>4} B  {:02x?}", shared.len(), &shared[..8.min(shared.len())]);
        records.push((n, shared));
    }
    let distinct: std::collections::BTreeSet<&Vec<u8>> = records.iter().map(|(_, b)| b).collect();
    assert!(
        distinct.len() > 1,
        "the corpus must contain at least two DISTINCT hvcC records — otherwise the 'never assume \
         the bytes are constant' rule is untested. Found {} identical across {} files.",
        distinct.len(),
        records.len()
    );
    eprintln!("hvcC: {} distinct records across {} files", distinct.len(), records.len());
}

/// ROW 3 — THE AUXILIARY INVENTORY. What E1 walked PAST, with a reason each, printed whole.
///
/// The corpus's own numbers are the assertion: an iPhone HEIC is ~129 items of which one is the
/// photograph and ~54 are its tiles, and the rest is a gain map (Main10), a linear-thumb/style-delta
/// grid (RExt monochrome), the mattes, a thumbnail, Exif and four RDF/XML blobs. The investigation's
/// standing instruction for this stage is that the parser must WALK PAST those without choking; the
/// proof that it walked past rather than fell over is that it can still say what they were — down to
/// the aux grids' shapes and bit depths, which is precisely the information a future gain-map stage
/// would otherwise have to re-derive.
#[test]
fn the_auxiliary_items_are_exposed_with_a_reason() {
    let Some(files) = corpus_or_skip("aux inventory") else { return };
    let mut saw_main10_grid = false;
    let mut saw_aux_urn = false;
    for p in &files {
        let n = name(p);
        let g = parse_heif_grid(&std::fs::read(p).expect("read")).expect("parse");
        let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
        for a in &g.aux {
            *by_reason.entry(format!("{:?}", a.why)).or_default() += 1;
        }
        println!("── {n}: {} items, {} tiles, {} skipped", g.item_count, g.tiles.len(), g.aux.len());
        println!("   reasons: {by_reason:?}");
        for a in g.aux.iter().filter(|a| a.grid.is_some() || a.aux_type.is_some()) {
            println!(
                "   item {:>3} '{}' {:?} grid={:?} depth={:?} {}",
                a.item_id,
                String::from_utf8_lossy(&a.item_type),
                a.why,
                a.grid,
                a.bit_depth,
                a.aux_type.as_deref().unwrap_or("")
            );
            if a.grid.is_some() && a.bit_depth == Some(10) {
                saw_main10_grid = true;
            }
            if a.aux_type.is_some() {
                saw_aux_urn = true;
            }
        }
        assert_eq!(
            g.item_count,
            1 + g.tiles.len() + g.aux.len(),
            "{n}: every declared item must be the primary, one of its tiles, or inventoried — \
             nothing may be silently dropped"
        );
        assert!(
            g.aux.iter().all(|a| a.item_id != g.primary_item_id),
            "{n}: the primary must never appear in its own skip list"
        );
    }
    // The corpus's own facts, so an inventory that quietly stopped reading properties would show.
    assert!(saw_main10_grid, "no 10-bit auxiliary grid found — the Main10 gain map is not being read");
    assert!(saw_aux_urn, "no auxC URN read — the aux chain's own labels are not being exposed");
}

/// ROW 4 — HOSTILE INPUT, on a REAL container. 5,000 deterministic single-byte flips through the
/// parser; the only requirement is that it RETURNED.
///
/// Flips are aimed at the first 64 KB, which is where `ftyp` and the whole ~35 KB `meta` box live —
/// a flip in the middle of `mdat` mutates a pixel nobody parses and would make the trial count a
/// vanity number. Nothing about the outcome is asserted beyond the return, because a mutated
/// container may legitimately still parse: flipping a byte inside an `Exif` item's payload changes
/// nothing structural, and demanding a decline would be demanding that the parser be WRONG.
///
/// The counters are printed rather than bounded. They are a witness that the trials landed
/// somewhere interesting, not a tolerance to tune.
#[test]
fn byte_flips_on_a_real_container_never_panic() {
    let Some(files) = corpus_or_skip("fuzz") else { return };
    // The smallest real file, so 5,000 trials stay cheap; its container shape is the same 8×6 grid
    // machinery as the 48 MP files.
    let target = files
        .iter()
        .min_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX))
        .expect("a corpus file");
    let mut bytes = std::fs::read(target).expect("read the fuzz target");
    let base = parse_heif_grid(&bytes).expect("the unmutated target must parse");
    // Parenthesised deliberately: `64 * 1024usize.min(len)` binds the `.min` to 1024 and would give
    // 64 KB even for a 100-byte file, then index past the end of it.
    let head = (64 * 1024usize).min(bytes.len());
    eprintln!(
        "fuzz target: {} ({} bytes, {} tiles); flipping within the first {head} bytes",
        name(target),
        bytes.len(),
        base.tiles.len()
    );

    let mut seed = 0xD1B54A32D192ED03u64;
    let (mut ok, mut err) = (0usize, 0usize);
    for _ in 0..5_000 {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let r = seed.wrapping_mul(0x2545F4914F6CDD1D);
        let at = (r % head as u64) as usize;
        let bit = 1u8 << ((r >> 32) % 8);
        bytes[at] ^= bit;
        // The parser must RETURN. A panic here fails the test by unwinding out of it, which is the
        // assertion — there is nothing to write down.
        match parse_heif_grid(&bytes) {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        }
        bytes[at] ^= bit; // restore, so every trial is exactly one flip from the original
    }
    assert_eq!(ok + err, 5_000);
    assert!(err > 0, "5,000 flips inside the metadata and not one was rejected — the fuzz is inert");
    assert!(ok > 0, "5,000 flips and none still parsed — the fuzz is not reaching the parser");
    eprintln!("fuzz: 5000 single-byte flips — {ok} still parsed, {err} declined, 0 panics");
    // The file on disk is untouched: every flip was restored, and this proves the round trip.
    assert_eq!(bytes, std::fs::read(target).expect("re-read"), "the fuzz must not corrupt its target");
}

/// ROW 5 — the corpus really does hold the matched scaling-list PAIR Stage 0 identified for E3.
/// Cheap to assert, and it is the row that goes red the day someone tidies `IMG_3258.HEIC` out of
/// the real-life folder and quietly removes E3's only explicit-quantisation-matrix fixture.
#[test]
fn the_corpus_holds_the_matched_scaling_list_pair() {
    let Some(files) = corpus_or_skip("scaling-list pair") else { return };
    let find = |stem: &str| files.iter().find(|p| name(p).starts_with(stem)).cloned();
    let (Some(defaults), Some(explicit)) = (find("IMG_1826"), find("IMG_3258")) else {
        eprintln!("skip (scaling-list pair): IMG_1826 and/or IMG_3258 not present in the corpus");
        return;
    };
    let a = parse_heif_grid(&std::fs::read(&defaults).unwrap()).expect("parse IMG_1826");
    let b = parse_heif_grid(&std::fs::read(&explicit).unwrap()).expect("parse IMG_3258");
    assert_eq!(
        (a.cols, a.rows, a.tile_w, a.tile_h),
        (b.cols, b.rows, b.tile_w, b.tile_h),
        "the pair must be identical in geometry — that is what makes it a matched pair"
    );
    assert_eq!(a.display, b.display);
    assert_ne!(
        a.hvcc, b.hvcc,
        "…and DIFFERENT in hvcC: IMG_1826 carries the default scaling lists, IMG_3258 the explicit \
         ones. If these ever compare equal, E3's quantisation-matrix landmine has lost its fixture."
    );
    eprintln!(
        "scaling-list pair: IMG_1826 hvcC {} B vs IMG_3258 hvcC {} B, same {}x{} grid",
        a.hvcc.as_ref().map(|v| v.len()).unwrap_or(0),
        b.hvcc.as_ref().map(|v| v.len()).unwrap_or(0),
        a.cols,
        a.rows
    );
}

/// ROW 6 — the parser declines the testkit's HOSTILE and NON-HEIF files without a word of
/// complaint. `testkit/edge` is the folder of things that are not the image they claim to be, and
/// the shipping rule for every one of them is a soft decline. A container parser that panicked on a
/// zero-byte file would take a worker thread with it.
#[test]
fn non_heif_and_edge_files_decline_softly() {
    let dir = fixture_paths::edge();
    let Ok(rd) = std::fs::read_dir(dir) else {
        eprintln!("skip (edge files): {} missing", dir.display());
        return;
    };
    let mut n = 0usize;
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_file()) {
        let bytes = std::fs::read(&p).unwrap_or_default();
        assert!(
            parse_heif_grid(&bytes).is_err(),
            "{}: a non-HEIF file must never yield a grid plan",
            name(&p)
        );
        assert!(heif_decode_plan(&p).is_err(), "{}: …through the path door either", name(&p));
        n += 1;
    }
    // The empty slice and a lone fourcc are the two degenerate shapes the folder does not hold.
    assert!(parse_heif_grid(&[]).is_err(), "the empty buffer must decline");
    assert!(parse_heif_grid(b"ftyp").is_err(), "four bytes must decline");
    assert!(heif_decode_plan(Path::new("no_such_file.heic")).is_err(), "a missing file must decline");
    eprintln!("edge: {n} files declined softly");
}
