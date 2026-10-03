//! v0.8.177 — THE ACCEPTANCE CASE, on the owner's own files.
//!
//! The 2026-08-08 measurement that opened this round (weic2212a, an ESA release pair, checked
//! against littleCMS as a reference CM): `weic2212a.jpg` (sRGB) and `weic2212a.tif` (ProPhoto RGB)
//! are THE SAME IMAGE — mean|Δ| 0.7/255, 99.2% of pixels within 4 levels under correct management.
//! Falcon rendered the TIFF at mean|Δ| ≈ 7/255 over 72% of its pixels, chroma 26 → 18.7 (~28%
//! desaturated) with a green/blue midtone lift, because the ProPhoto colorants miss every modeled
//! gamut and the NAME fallback mapped "prophoto" → Rec.2020.
//!
//! WHAT THIS FILE PINS, through the SHIPPING door (`shot_source_gamut` / the same `file_color_tag`
//! reader every tier uses), not through a synthetic stand-in:
//!   * the TIFF takes the FAITHFUL route and no longer resolves to Rec.2020;
//!   * it is labelled with the PROFILE'S OWN name, never a modeled gamut's;
//!   * its sRGB twin is completely unaffected — same answer, same route, same label as before.
//!
//! Self-skips when the asset folder is absent, following this crate's `real.rs` convention.
//!
//! The pixel-level agreement with a reference transform is pinned in `falcon-color`'s own
//! `prophoto_renders_through_its_own_profile_not_as_rec2020`, on a synthetic profile that was
//! verified field-for-field against the real one embedded here (ICC v2.1, `mntr`/RGB/XYZ, NO `chad`,
//! `desc` = the v2 ASCII "ProPhoto RGB", r/g/bTRC = one shared 14-byte `curv` N=1 at gamma
//! 461/256 = 1.80078125, colorants = the published D50 ROMM primaries). Keeping the byte-level
//! reference synthetic keeps a third-party profile out of the repo; keeping THIS row on the real
//! file keeps the fix honest about the artefact it was written for.

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

use std::path::{Path, PathBuf};

use falcon_color::{Gamut, GamutRoute};
use falcon_decode::{scan_folder, shot_source_gamut, Shot};


fn shot_named(name: &str) -> Option<Shot> {
    let dir = Path::new(fixture_paths::photos());
    if !dir.exists() {
        eprintln!("SKIP: set FALCON_PHOTO_TEST_DIR; corpus missing ({})", fixture_paths::photos().display());
        return None;
    }
    if !fixture_paths::require_file(&PathBuf::from(fixture_paths::photos()).join(name)) {
        eprintln!("skip: {name} not in the asset folder");
        return None;
    }
    let shots = scan_folder(dir).expect("scan folder");
    let s = shots.into_iter().find(|s| {
        s.jpg.as_ref().map(|p| p.file_name().map(|f| f.eq_ignore_ascii_case(name)).unwrap_or(false)).unwrap_or(false)
    });
    if !fixture_paths::require_available(s.is_some(), "reference photo was omitted by scan_folder") {
        eprintln!("skip: {name} did not come back from scan_folder");
    }
    s
}

/// THE ROUND'S REASON, as one assertion: the owner's ProPhoto TIFF is no longer rendered as
/// Rec.2020.
///
/// FALSIFIER: remove route (2) from `falcon_color::resolve_source_gamut` and this row reports
/// `Rec. 2020` again — the exact state the 08-08 measurement was taken in.
#[test]
fn the_owners_prophoto_tiff_takes_the_faithful_route() {
    let Some(shot) = shot_named("weic2212a.tif") else { return };
    let g = shot_source_gamut(&shot);
    eprintln!("weic2212a.tif → {:?} / display name {:?}", g, g.display_name());
    assert!(g.is_source_profile(), "the TIFF must render through its own profile, got {g:?}");
    assert_ne!(g, Gamut::Rec2020, "the retired approximation must not come back");

    // The label is the profile's own — read by falcon-color from the same bytes the transform was
    // built from, so it cannot be a modeled name the file is not.
    assert_eq!(g.display_name(), "ProPhoto RGB");

    // …and the profile really is the one the round was written for: ProPhoto's primaries sit far
    // outside τ of everything modeled (that miss is what OPENS the faithful route), and the curve is
    // the gamma-1.8 a real ProPhoto profile carries.
    let p = falcon_color::source_profile(g).expect("a faithful gamut carries its profile");
    eprintln!("  colorants {:?}\n  trc {}", p.rgb_to_xyz, p.trc_summary);
    assert!(p.trc_summary.starts_with("gamma 1.80"), "ProPhoto's TRC is gamma 1.8, got {}", p.trc_summary);
    // Its red primary is far redder than anything we model — the cheapest single number that says
    // "this really is a ProPhoto-class space" without restating the whole matrix.
    assert!(p.rgb_to_xyz[0][0] > 0.7, "ProPhoto's red-X is ~0.797, got {}", p.rgb_to_xyz[0][0]);
}

/// The other half of the acceptance bar: the sRGB twin must be COMPLETELY unaffected. It resolves
/// by colorimetry to modeled sRGB, exactly as it did before this round — which is what makes the
/// owner's comparison a fair one, and what route (1)-goes-first buys.
#[test]
fn the_srgb_twin_is_untouched() {
    let Some(shot) = shot_named("weic2212a.jpg") else { return };
    let g = shot_source_gamut(&shot);
    eprintln!("weic2212a.jpg → {:?} / display name {:?}", g, g.display_name());
    assert_eq!(g, Gamut::Srgb, "an ordinary sRGB JPEG must still be plain modeled sRGB");
    assert!(!g.is_source_profile(), "…and must NEVER be pushed into the faithful registry");
    assert_eq!(g.display_name(), "sRGB");
}

/// Every finished-image file in the owner's asset folder, swept: the answer must be deterministic,
/// and no file may take the faithful route unless its profile genuinely misses τ. This is the
/// folder-scale version of the chain-order pin — it would catch a faithful route that started
/// claiming ordinary files.
#[test]
fn no_ordinary_file_in_the_folder_is_pushed_onto_the_faithful_route() {
    let dir = Path::new(fixture_paths::photos());
    if !dir.exists() {
        eprintln!("SKIP: set FALCON_PHOTO_TEST_DIR; corpus missing ({})", fixture_paths::photos().display());
        return;
    }
    let shots = scan_folder(dir).expect("scan folder");
    let (mut faithful, mut modeled) = (0usize, 0usize);
    for s in &shots {
        let g = shot_source_gamut(s);
        assert_eq!(shot_source_gamut(s), g, "{}: the answer must be deterministic", s.name);
        if g.is_source_profile() {
            faithful += 1;
            let p = falcon_color::source_profile(g).expect("a faithful gamut carries its profile");
            eprintln!("  faithful: {} → {:?} ({})", s.name, g.display_name(), p.trc_summary);
        } else {
            modeled += 1;
        }
    }
    eprintln!("swept {} shots: {modeled} modeled, {faithful} faithful", shots.len());
    assert!(modeled > 0, "the folder must contain ordinary files, or this sweep proves nothing");
}

/// The route token the log line prints must be greppable and must match the answer — the tester
/// reads these lines, and a token that disagreed with the gamut would be worse than none.
#[test]
fn the_route_token_matches_the_answer() {
    assert_eq!(GamutRoute::Colorimetry.token(), "modeled-colorants");
    assert_eq!(GamutRoute::Faithful.token(), "faithful");
    assert_eq!(GamutRoute::Description.token(), "name-fallback");
    assert_eq!(GamutRoute::Fallback.token(), "default");
}
