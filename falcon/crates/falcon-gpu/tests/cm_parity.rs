//! LV3 (§6.0) parity coverage for the relocated single-source colour-manage shader
//! `falcon_gpu::CM_SHADER`. Two things are asserted:
//!
//!  1. **Custom-vs-named parity of the kind-2 encode path** — the historically-fragile bit. A
//!     `Gamut::Custom` profile carrying Adobe RGB's primaries + gamma must colour-manage IDENTICALLY to
//!     the named `Gamut::AdobeRgb`. v0.8.47: the Custom encode now goes through the shader's `kind == 2`
//!     branch, which samples the per-channel INVERSE-TONE-CURVE LUT (`lut_tex`, `textureLoad`+`mix`); the
//!     named Adobe encode is the analytic `kind == 1` (`pow(x, 1/γ)`). The LUT must reconstruct Adobe's
//!     gamma so the fast (GPU) and detail (CPU) tiers stay seam-free. This is the mechanism the
//!     "fast == full colour" contract rides on.
//!
//!  2. **Single source** — support.rs references THIS `pub const`, so byte-equality between the two prod
//!     copies is true by construction; a shape assertion documents the contract (incl. the LUT hook).
//!
//! FALLBACK LEVEL (stated plainly): this is the CPU-reference check, NOT a live headless GPU render. The
//! production CM pipeline builder (`ColorTransform` / `create_texture_cm`) lives in the app **binary**
//! crate (native/src/support.rs), so a falcon-gpu integration test cannot drive it. Instead we mirror the
//! shader's `linearize`/`encode` maths EXACTLY on the CPU — including the kind-2 LUT fetch, driven from
//! the SAME u16 LUT `falcon_color::custom_encode_lut` hands the GPU texture — and cross-check it against
//! falcon-color's production `transform_rgba`. So the check exercises the actual colour maths the WGSL
//! encodes, on the same data. No GPU required; never skips.

use falcon_color::{
    custom_encode_lut, mat3_inv, mat3_mul, set_custom_profile, src_to_dst_matrix, CustomProfile, Gamut,
    ADOBE_GAMMA, CUSTOM_LUT_N,
};

// CPU mirrors of the CM_SHADER WGSL `linearize` / `encode`. The source TRC never uses kind 2
// (`Gamut::Custom` is a DESTINATION-only target), so `linearize` handles sRGB-piecewise (kind 0),
// Adobe pure-gamma (kind 1) and — v0.8.177 — a FAITHFUL SOURCE profile's per-channel forward LUT
// (kind 4), exactly like the shader. Kind 4 reads rows 3..5 of the same texture kind 2 reads rows
// 0..2 of, which is why `src_lut` is a separate slice here: the mirror must not be able to reach the
// destination rows by accident, or it would agree with a shader that does the wrong fetch.
fn linearize(c: f32, kind: u32, ch: usize, src_lut: Option<&[u16]>) -> f32 {
    if kind == 4 {
        return lut_sample_mirror(src_lut.expect("kind-4 linearise needs the source LUT"), ch, c);
    }
    if kind == 1 {
        return c.max(0.0).powf(ADOBE_GAMMA);
    }
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Byte-twin of the WGSL kind-2 encode: `fpos = x*(N-1)`, load texels `i0`,`i1` from row `ch`, `mix`.
/// The GPU texture stores `u16/65535` as R32Float, so `textureLoad` returns exactly these values.
fn lut_sample_mirror(lut: &[u16], ch: usize, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let fpos = x * (CUSTOM_LUT_N - 1) as f32;
    let i0 = fpos.floor() as usize;
    let i1 = (i0 + 1).min(CUSTOM_LUT_N - 1);
    let frac = fpos - fpos.floor();
    let base = ch * CUSTOM_LUT_N;
    let v0 = lut[base + i0] as f32 / 65535.0;
    let v1 = lut[base + i1] as f32 / 65535.0;
    v0 + (v1 - v0) * frac
}

fn encode(lut: Option<&[u16]>, kind: u32, ch: usize, c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    if kind == 2 {
        return lut_sample_mirror(lut.expect("kind-2 encode needs the custom LUT"), ch, x);
    }
    if kind == 1 {
        return x.powf(1.0 / ADOBE_GAMMA);
    }
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

// The full CM transform, mirroring the shader fragment: linearise src TRC → src→dst matrix → encode dst TRC.
fn cpu_cm(px: [f32; 3], src: Gamut, dst: Gamut, lut: Option<&[u16]>) -> [f32; 3] {
    cpu_cm_full(px, src, dst, lut, None)
}

fn cpu_cm_full(px: [f32; 3], src: Gamut, dst: Gamut, lut: Option<&[u16]>, src_lut: Option<&[u16]>) -> [f32; 3] {
    let st = src.trc_kind();
    let dt = dst.trc_kind();
    let m = src_to_dst_matrix(src, dst);
    let l = [
        linearize(px[0], st, 0, src_lut),
        linearize(px[1], st, 1, src_lut),
        linearize(px[2], st, 2, src_lut),
    ];
    let mut out = [0.0f32; 3];
    for (r, o) in out.iter_mut().enumerate() {
        let lin = m[r][0] * l[0] + m[r][1] * l[1] + m[r][2] * l[2];
        *o = encode(lut, dt, r, lin);
    }
    out
}

/// Recover Adobe RGB's (private) linear-RGB→XYZ colorant matrix using ONLY falcon-color's public API — no
/// hardcoded colour constants. First recover sRGB→XYZ: install an IDENTITY Custom profile so
/// `src_to_dst_matrix(Custom, Srgb) == inv(sRGB→XYZ)·I`, then invert. Then
/// `Adobe→XYZ == sRGB→XYZ · src_to_dst_matrix(Adobe, Srgb)` (the internal inverse cancels). Tracks
/// falcon-color's real matrices automatically, so it can never drift stale.
fn recover_adobe_rgb_to_xyz() -> [[f32; 3]; 3] {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    set_custom_profile(CustomProfile::from_gamma(identity, 2.2, "id"));
    let srgb_to_xyz = mat3_inv(src_to_dst_matrix(Gamut::Custom, Gamut::Srgb));
    mat3_mul(srgb_to_xyz, src_to_dst_matrix(Gamut::AdobeRgb, Gamut::Srgb))
}

#[test]
fn custom_lut_matches_equivalent_named_gamut() {
    // Build a Custom profile equivalent to Adobe RGB: Adobe's exact primaries (recovered via public API)
    // and its pure display gamma (= ADOBE_GAMMA). `from_gamma` fills the inverse LUT with the analytic
    // `x^(1/γ)`. Installed process-globally; read by `Gamut::Custom`.
    let adobe_to_xyz = recover_adobe_rgb_to_xyz();
    set_custom_profile(CustomProfile::from_gamma(adobe_to_xyz, ADOBE_GAMMA, "test-adobe-equivalent"));

    // TRC families: named Adobe = kind 1 (analytic), Custom = kind 2 (LUT) — the two encode branches.
    assert_eq!(Gamut::AdobeRgb.trc_kind(), 1);
    assert_eq!(Gamut::Custom.trc_kind(), 2);

    // The kind-2 LUT the GPU texture is built from.
    let (_, lut) = custom_encode_lut().expect("custom profile installed → LUT exposed");

    // Matrix parity: Custom now carries Adobe's primaries, so the src→dst matrices coincide.
    let mc = src_to_dst_matrix(Gamut::Srgb, Gamut::Custom);
    let ma = src_to_dst_matrix(Gamut::Srgb, Gamut::AdobeRgb);
    for (rc, ra) in mc.iter().zip(ma.iter()) {
        for (vc, va) in rc.iter().zip(ra.iter()) {
            assert!((vc - va).abs() < 1e-4, "matrix parity: Custom {vc} vs Adobe {va}");
        }
    }

    // Headline: drive a gradient through the full CM path twice — sRGB→AdobeRgb (kind 1 analytic) and
    // sRGB→Custom (kind 2 LUT) — and assert the 8-bit outputs match within ±1. Any divergence in the
    // kind-2 LUT encode (the fast==full colour seam) shows up here.
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as i32;
    for i in 0..=32u32 {
        let c = i as f32 / 32.0;
        let px = [c, c * 0.5, 1.0 - c];
        let named = cpu_cm(px, Gamut::Srgb, Gamut::AdobeRgb, None);
        let custom = cpu_cm(px, Gamut::Srgb, Gamut::Custom, Some(&lut));
        for ch in 0..3 {
            assert!(
                (q(named[ch]) - q(custom[ch])).abs() <= 1,
                "gradient step {i} ch{ch}: named {} vs custom(LUT) {}",
                named[ch],
                custom[ch]
            );
        }
    }
}

#[test]
fn shader_lut_mirror_matches_cpu_transform() {
    // The CPU-production path is `falcon_color::transform_rgba`; the GPU path is the WGSL `encode` LUT
    // fetch. This test drives the SAME LUT through the shader-mirror `cpu_cm` and cross-checks it against
    // production `transform_rgba` on a set of 8-bit probes — so a drift between the two encoders (a scrub
    // ↔ detail colour seam) fails here. Includes the shadow probes the v0.8.47 fix targets.
    let adobe_to_xyz = recover_adobe_rgb_to_xyz();
    set_custom_profile(CustomProfile::from_gamma(adobe_to_xyz, ADOBE_GAMMA, "mirror"));
    let (_, lut) = custom_encode_lut().expect("LUT exposed");

    for &(r, g, b) in &[
        (16u8, 16, 16),
        (32, 32, 32),
        (64, 64, 64),
        (128, 128, 128),
        (200, 90, 40),
        (10, 200, 250),
        (0, 0, 0),
        (255, 255, 255),
    ] {
        // Production CPU.
        let mut px = [r, g, b, 255u8];
        falcon_color::transform_rgba(&mut px, Gamut::Srgb, Gamut::Custom);
        // Shader mirror (float in, quantise out the same way apply_pixel does).
        let m = cpu_cm([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0], Gamut::Srgb, Gamut::Custom, Some(&lut));
        let mq = [
            (m[0] * 255.0 + 0.5) as u8,
            (m[1] * 255.0 + 0.5) as u8,
            (m[2] * 255.0 + 0.5) as u8,
        ];
        for ch in 0..3 {
            assert!(
                (px[ch] as i32 - mq[ch] as i32).abs() <= 1,
                "CPU transform vs shader-LUT mirror at ({r},{g},{b}) ch{ch}: {} vs {}",
                px[ch],
                mq[ch]
            );
        }
    }
}

#[test]
fn cm_shader_is_single_source() {
    // support.rs builds its CM pipeline from THIS same const (falcon_gpu::CM_SHADER), so the two prod
    // copies are byte-identical by construction. Assert the shader's shape so the contract can't silently
    // rot: the fragment entry, the shared TRC helpers, and — critically — the kind-2 Custom LUT hook.
    let s = falcon_gpu::CM_SHADER;
    assert!(s.contains("fn fs("), "CM_SHADER missing fragment entry");
    assert!(s.contains("fn linearize("), "CM_SHADER missing linearize");
    assert!(s.contains("fn encode("), "CM_SHADER missing encode");
    // The kind-2 (Custom) encode branch now samples the inverse-LUT texture — the v0.8.47 parity mechanism.
    assert!(s.contains("kind == 2u"), "CM_SHADER missing the Custom (kind 2) encode branch");
    assert!(s.contains("textureLoad(lut_tex"), "CM_SHADER kind-2 encode must fetch the inverse-LUT texture");
    assert!(s.contains("@binding(3) var lut_tex"), "CM_SHADER missing the lut_tex binding");
    assert!(!s.contains("bitcast<f32>(P.flags.w)"), "CM_SHADER still single-gamma (bitcast flags.w)");
}

/// v0.8.177 — THE SOURCE HALF OF THE SAME CONTRACT. A faithful source profile is linearised on the
/// GPU by the kind-4 branch (a `textureLoad`+`mix` of rows 3..5 of `lut_tex`) and on the CPU by
/// `Trc::SrcLut`. Both read the SAME `u16` table — the one `falcon_color::source_linearize_lut`
/// hands the texture builder — so this mirrors the shader's fetch and cross-checks it against
/// production `transform_rgba`.
///
/// This is the row that would catch a scrub↔detail colour seam on a ProPhoto photograph: if the
/// shader's kind-4 arm were missing, or fetched the wrong rows, or the CPU sampled the curve
/// analytically instead of through the table, the two would diverge here.
///
/// No GPU required, and it never skips — same fallback level as the kind-2 row above.
#[test]
fn faithful_source_lut_mirror_matches_cpu_transform() {
    // A ProPhoto-class profile: published ROMM primaries (D50, no `chad` — the ICC v2 working-space
    // shape, which is what the owner's real TIFF carries) and a gamma-1.8 `curv`.
    let icc = prophoto_v2_icc();
    let src = falcon_color::register_source_profile(&icc).expect("a matrix-TRC source registers");
    assert_eq!(src.trc_kind(), 4, "a faithful source is GPU kind 4");
    let (_, src_lut) = falcon_color::source_linearize_lut(src).expect("it exposes its forward LUT");
    assert_eq!(src_lut.len(), 3 * CUSTOM_LUT_N, "…in the layout the texture's rows 3..5 hold");

    for dst in [Gamut::Srgb, Gamut::AdobeRgb] {
        for &(r, g, b) in &[
            (0u8, 0, 0),
            (1, 1, 1),
            (16, 16, 16),
            (64, 32, 200),
            (128, 128, 128),
            (200, 90, 40),
            (255, 255, 255),
        ] {
            let mut px = [r, g, b, 255u8];
            falcon_color::transform_rgba(&mut px, src, dst);
            let m = cpu_cm_full(
                [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
                src,
                dst,
                None,
                Some(&src_lut),
            );
            for ch in 0..3 {
                let mq = (m[ch] * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                assert!(
                    (px[ch] as i32 - mq as i32).abs() <= 1,
                    "CPU transform vs shader kind-4 mirror at ({r},{g},{b}) → {dst:?} ch{ch}: {} vs {mq}",
                    px[ch]
                );
            }
        }
    }
}

/// A minimal ICC v2 ProPhoto-RGB profile: the published D50 ROMM colorants, no `chad` (so the parser
/// Bradford-adapts them to D65, as it does for the owner's real file), and one shared gamma-1.8
/// `curv` on all three channels.
fn prophoto_v2_icc() -> Vec<u8> {
    const PROPHOTO_D50: [[f32; 3]; 3] = [
        [0.797_675, 0.135_192, 0.031_353],
        [0.288_040, 0.711_874, 0.000_086],
        [0.000_000, 0.000_000, 0.825_210],
    ];
    let s15 = |v: f32| (((v as f64) * 65536.0).round() as i32).to_be_bytes();
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
        v.extend_from_slice(&461u16.to_be_bytes()); // u8Fixed8 gamma 1.80078125
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
