//! v0.8.177 — the faithful-source registry's CEILING, in its own test binary.
//!
//! WHY A SEPARATE BINARY. The registry is process-global and APPEND-ONLY (a `Gamut::SourceIcc`
//! index must stay valid for as long as any frame stamp or RAM-cache entry holds that gamut, so
//! entries are never replaced or evicted). A test that fills it to `SOURCE_PROFILE_CAP` therefore
//! cannot undo itself, and inside `lib.rs`'s unit-test binary it would starve every other row that
//! registers a profile — an order-dependent, parallelism-dependent failure of exactly the kind that
//! wastes an afternoon. Cargo gives each integration test file its own process; this one's registry
//! is its own.
//!
//! WHAT IT PINS. Past the cap a profile does not get a wrong-but-confident answer and does not get a
//! half-registered one: `register_source_profile` returns `Err(FaithfulRefusal::RegistryFull)` and
//! `resolve_source_gamut` falls through to the routes that existed before this round — the name,
//! then sRGB, carrying the refusal so the log can say WHICH cause applied. The degradation is the
//! PRE-v0.8.177 behaviour, which is the only safe thing for a ceiling to degrade to.
//!
//! FALSIFIER: remove the `reg.len() >= SOURCE_PROFILE_CAP` guard from `register_source_profile` and
//! `the_registry_stops_growing_at_the_cap` reddens on the count — and a folder of crafted profiles
//! would grow the registry without bound at ~32 KB apiece.

use falcon_color::{
    register_source_profile, resolve_source_gamut, source_profile, FaithfulRefusal, Gamut, GamutRoute,
    SOURCE_PROFILE_CAP,
};

/// An s15Fixed16 big-endian word.
fn s15(v: f32) -> [u8; 4] {
    (((v as f64) * 65536.0).round() as i32).to_be_bytes()
}

/// A minimal matrix-TRC profile: ProPhoto-class colorants (far from every modeled gamut, so it
/// always misses τ and always reaches route 2) with a single-gamma `curv`. `salt` perturbs the
/// gamma so every profile is byte-distinct and cannot dedup with its neighbours.
fn distinct_matrix_icc(salt: u16) -> Vec<u8> {
    const PROPHOTO_D50: [[f32; 3]; 3] = [
        [0.797_675, 0.135_192, 0.031_353],
        [0.288_040, 0.711_874, 0.000_086],
        [0.000_000, 0.000_000, 0.825_210],
    ];
    let xyz = |col: usize| {
        let mut v = b"XYZ ".to_vec();
        v.extend_from_slice(&[0u8; 4]);
        for row in PROPHOTO_D50 {
            v.extend_from_slice(&s15(row[col]));
        }
        v
    };
    let curv = {
        let mut v = b"curv".to_vec();
        v.extend_from_slice(&[0u8; 4]);
        v.extend_from_slice(&1u32.to_be_bytes());
        // 256..=~2000 in u8Fixed8 → gamma 1.0..~7.8; distinct per salt, always a valid curve.
        v.extend_from_slice(&(300u16 + salt).to_be_bytes());
        v
    };
    let entries: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"rXYZ", xyz(0)),
        (*b"gXYZ", xyz(1)),
        (*b"bXYZ", xyz(2)),
        (*b"rTRC", curv.clone()),
        (*b"gTRC", curv.clone()),
        (*b"bTRC", curv),
    ];
    let base = 128 + 4 + entries.len() * 12;
    let (mut data, mut offs) = (Vec::new(), Vec::new());
    for (_s, d) in &entries {
        offs.push(base + data.len());
        data.extend_from_slice(d);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    let mut icc = vec![0u8; 128];
    icc.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (sig, d)) in entries.iter().enumerate() {
        icc.extend_from_slice(sig);
        icc.extend_from_slice(&(offs[i] as u32).to_be_bytes());
        icc.extend_from_slice(&(d.len() as u32).to_be_bytes());
    }
    icc.extend_from_slice(&data);
    icc
}

#[test]
fn the_registry_stops_growing_at_the_cap_and_degrades_to_the_older_routes() {
    // Fill it exactly. Every one of these is a distinct, valid, unplaceable matrix-TRC profile.
    for k in 0..SOURCE_PROFILE_CAP {
        let icc = distinct_matrix_icc(k as u16);
        let g = register_source_profile(&icc)
            .unwrap_or_else(|e| panic!("profile {k} is below the cap and must register, got {e:?}"));
        assert!(g.is_source_profile());
        assert!(source_profile(g).is_some(), "a registered gamut resolves to its profile");
    }

    // One past it: refused, and — the point of the row — the FILE still renders, by the routes that
    // existed before v0.8.177.
    let over = distinct_matrix_icc(SOURCE_PROFILE_CAP as u16);
    assert_eq!(register_source_profile(&over), Err(FaithfulRefusal::RegistryFull), "the cap must hold");

    let named = resolve_source_gamut(Some(&over), Some("ProPhoto RGB"));
    assert_eq!(named.route, GamutRoute::Description, "past the cap the NAME answers again");
    assert_eq!(named.gamut, Gamut::Rec2020, "…with the documented approximation, not a wrong-confident one");

    let unnamed = resolve_source_gamut(Some(&over), Some("something we do not model"));
    assert_eq!(unnamed.route, GamutRoute::Fallback, "and with no usable name, the sRGB floor");
    assert_eq!(unnamed.gamut, Gamut::Srgb);

    // v0.8.177 (G-O4): the line a capped-out file writes says CAP, not the catch-all — the one fact
    // that distinguishes "restart and this renders faithfully" from "this profile never will".
    for r in [named, unnamed] {
        assert_eq!(r.faithful_refusal, Some(FaithfulRefusal::RegistryFull), "the cause travels with the answer");
    }
    assert_eq!(named.why(), "name — measured, unplaceable, faithful registry full");
    assert_eq!(unnamed.why(), "sRGB default — faithful registry full, name unrecognised");

    // The measurement is still reported honestly either way — the miss distance survives, so the
    // "measurable but unplaceable" telemetry still fires for a capped-out profile.
    assert!(named.nearest.is_some(), "the colorants were read; only the registration was refused");

    // An ALREADY-registered profile keeps working past the cap (the dedup lookup precedes the
    // ceiling check) — otherwise a full registry would break the files that filled it.
    let first = distinct_matrix_icc(0);
    let g = register_source_profile(&first).expect("an existing entry is still found when full");
    assert_eq!(g, Gamut::SourceIcc(0));
}
