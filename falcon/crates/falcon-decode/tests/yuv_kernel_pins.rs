//! v0.8.145 (E2) — **the CPU half of the new path's byte-pin discipline.**
//!
//! The JPEG byte-pin machinery is untouched by this round; this is a parallel set with its own
//! digests, its own inputs and its own falsifiers, because the hardware HEIC path is a different
//! pipeline that happens to end in the same packed-RGB8 contract.
//!
//! Every row of `falcon_decode::yuv_kernel::GOLDEN_PINS` is run through the CPU reference and its
//! SHA-256 asserted. The GPU twin (`falcon-gpu/tests/yuv_kernel_twin.rs`) iterates the SAME table
//! and asserts the SAME digests, plus byte-equality with this side — so "the twins agree" and "the
//! twins are right" are two independent failures.
//!
//! The digests are real SHA-256 over the packed RGB8 buffer, so any of them can be re-derived with
//! `sha256sum` on a dumped file (`cargo run -p falcon-decode --example yuv_kernel_probe -- --dump`)
//! with no Falcon code in the loop.

use falcon_decode::yuv_kernel::*;

/// Load the optional private tiles at runtime; public synthetic coverage needs no photo files.
#[path = "../../test-support/private_tiles.rs"]
mod private_tiles;
fn fixtures() -> private_tiles::PrivateTiles { private_tiles::load() }

/// **The pin.** Every golden row, printed whole before it is judged, then asserted.
///
/// FALSIFIER (run and restored during the v0.8.145 round): flip `ROUND_ADDEND` from `1 << 19` to 0
/// in `yuv_kernel.rs` and every one of these rows reddens with a different digest — which is what
/// says the pins are over the real arithmetic and not over a constant.
#[test]
fn the_golden_digests_hold_on_the_cpu_reference() {
    let tiles = fixtures();
    let fx = tiles.refs();
    let mut bad = Vec::new();
    println!("{:<40} {:>10} {:>4}x{:<4} sha256", "case", "bytes", "w", "h");
    for c in tiles.cases() {
        let rgb = yuv_golden_cpu_rgb(c, &fx);
        assert_eq!(rgb.len(), (c.w * c.h * 3) as usize, "{}: output must be packed RGB8 w*3", c.name);
        let d = sha256_hex(&rgb);
        println!("{:<40} {:>10} {:>4}x{:<4} {d}", c.name, rgb.len(), c.w, c.h);
        if d != c.digest {
            bad.push(format!("  {:<40} pinned {} got {d}", c.name, c.digest));
        }
    }
    assert!(bad.is_empty(), "golden digest drift on {} row(s):\n{}", bad.len(), bad.join("\n"));
}

/// The pins must be DISTINCT. Twenty-three rows that all hashed the same thing would still "pass"
/// the digest test above while proving nothing about the parameter space.
#[test]
fn every_golden_row_produces_a_distinct_image() {
    let mut seen: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for c in GOLDEN_PINS {
        if let Some(prev) = seen.insert(c.digest, c.name) {
            panic!("{} and {} pin the SAME digest {} — one of them is not testing what it claims", prev, c.name, c.digest);
        }
    }
    assert_eq!(seen.len(), GOLDEN_PINS.len());
}

/// The real-tile rows must be pinned at the parameters the FILES ACTUALLY DECLARE, and the corpus
/// declares BT.601 / FULL / LEFT on all six files (SPS VUI, read not assumed — see the module
/// docs). A pin set that only ever exercised the file's real parameters through synthetic data
/// would leave the one combination that ships untested on real pixels.
#[test]
fn the_real_tiles_are_pinned_at_their_declared_parameters() {
    let declared = params_from_vui(6, true, None).expect("matrix 6 / full range / absent chroma_loc");
    let n = GOLDEN_PINS
        .iter()
        .filter(|c| matches!(c.src, YuvGoldenSrc::Fixture(_)) && c.params == declared)
        .count();
    assert!(n >= 2, "both real tiles must be pinned at the corpus's declared BT.601/full/left, got {n}");
}

/// The real-tile fixtures are the bytes this round measured. If one is ever swapped, replaced or
/// truncated, the kernel digests would move and the reader would have no way to tell whether the
/// KERNEL changed or the INPUT did. Pinning the input separately separates those two failures.
#[test]
fn the_real_tile_fixtures_are_the_bytes_this_round_measured() {
    let want = [
        ("heic_IMG_1826_t22_256x256.nv12", "eee450bba6379df675e8b11a2d42c710bbfc5e56ff7272911723b6b8c744abf0"),
        ("heic_IMG_3258_t40_256x256.nv12", "1ae806fcb58d1c32b49f8dc97d312d34718b9073b9c47135afcd1727c3dccef8"),
    ];
    let tiles = fixtures();
    let fx = tiles.refs();
    if tiles.is_empty() { return; } // optional private rows were reported by the loader
    assert_eq!(fx.len(), REAL_TILE_FIXTURES.len(), "every declared fixture must be included here");
    for (name, digest) in want {
        let bytes = fx.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("missing {name}")).1;
        assert_eq!(bytes.len(), 256 * 256 + 2 * 128 * 128, "{name} is a 256x256 NV12 surface");
        assert_eq!(sha256_hex(bytes), digest, "{name} is not the file this round measured");
    }
}

/// Real camera data must actually exercise the kernel — a fixture of flat sky would pass every
/// digest while testing almost nothing. Both excerpts were selected as the highest-detail 256×256
/// block of their file's highest-payload tile; this asserts the property that selection was for.
#[test]
fn the_real_tile_fixtures_carry_real_dynamic_range() {
    let tiles = fixtures();
    let fx = tiles.refs();
    for (name, bytes) in &fx {
        let y = &bytes[..256 * 256];
        let uv = &bytes[256 * 256..];
        let distinct_y: std::collections::BTreeSet<u8> = y.iter().copied().collect();
        let distinct_u: std::collections::BTreeSet<u8> = uv.iter().step_by(2).copied().collect();
        let distinct_v: std::collections::BTreeSet<u8> = uv.iter().skip(1).step_by(2).copied().collect();
        println!("{name}: distinct Y {} U {} V {}", distinct_y.len(), distinct_u.len(), distinct_v.len());
        assert_eq!(distinct_y.len(), 256, "{name} must carry every luma code");
        assert!(distinct_u.len() >= 32 && distinct_v.len() >= 32, "{name} chroma is too flat to test with");
    }
}

/// The kernel must not panic on ANY byte pattern. 4 000 randomly-filled NV12 frames across the odd
/// and even dimension classes and the whole parameter space — the same fail-closed discipline E1's
/// `heif_grid_fuzz_survives_byte_flips` applies to the container.
#[test]
fn kernel_fuzz_never_panics_and_always_fills_the_output() {
    let mut seed = 0x243F6A8885A308D3u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed.wrapping_mul(0x2545F4914F6CDD1D)
    };
    let dims = [(1u32, 1u32), (1, 7), (7, 1), (3, 5), (8, 8), (9, 9), (16, 3), (33, 17)];
    let mats = [YuvMatrix::Bt601, YuvMatrix::Bt709];
    let rngs = [YuvRange::Full, YuvRange::Limited];
    let sits = [ChromaSiting::Left, ChromaSiting::Center];
    let mut n = 0usize;
    for i in 0..4000u32 {
        let (w, h) = dims[(i as usize) % dims.len()];
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let y: Vec<u8> = (0..w * h).map(|_| (rnd() >> 24) as u8).collect();
        let uv: Vec<u8> = (0..2 * cw * ch).map(|_| (rnd() >> 24) as u8).collect();
        let p = YuvParams {
            matrix: mats[(rnd() % 2) as usize],
            range: rngs[(rnd() % 2) as usize],
            siting: sits[(rnd() % 2) as usize],
        };
        let rgb = nv12_to_rgb8(&Nv12Frame::packed(&y, &uv, w, h), p).expect("a well-formed frame converts");
        assert_eq!(rgb.len(), (w * h * 3) as usize);
        n += 1;
    }
    assert_eq!(n, 4000, "the fuzz loop must actually have run");
}
