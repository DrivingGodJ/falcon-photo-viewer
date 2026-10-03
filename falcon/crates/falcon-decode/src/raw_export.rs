//! CPU development for manufactured exports. This deliberately does not reuse the viewer's
//! RGB8/GPU/preview fallback paths. Rawler's initial decode allocations are outside Falcon's
//! general source cap except for the DNG header preflight below. Every decoded source is checked
//! before the substantially larger float development intermediates are allocated. This is a
//! dimension guard, not a promise that a RAW development occupies only one RGB frame of memory.

use anyhow::{bail, Context, Result};
use rawler::decoders::{Decoder, FormatHint, RawDecodeParams, WellKnownIFD};
use rawler::formats::tiff::Value;
use rawler::imgop::develop::{Intermediate, RawDevelop};
use rawler::imgop::{matrix, xyz::Illuminant, Rect};
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};

use crate::{guard_source_dims, Keep, Pixels, Shot};

fn is_xtrans(raw: &RawImage) -> bool {
    matches!(&raw.photometric, RawPhotometricInterpretation::Cfa(c) if c.cfa.width == 6 && c.cfa.height == 6)
}

/// Preserve rawler's default Bayer viewer processing while sharing the explicit high-quality
/// X-Trans developer with export. One decompression per invocation; no preview substitution.
pub(crate) fn develop_raw_image_for_viewer(path: &std::path::Path) -> Result<image::DynamicImage> {
    contain_panic(|| {
        require_downloaded(path)?;
        let source = RawSource::new(path)?;
        let decoder = rawler::get_decoder(&source)?;
        let mut raw = load_viewer_raw(decoder.as_ref(), || {
            Ok(decoder.raw_image(&source, &RawDecodeParams::default(), false)?)
        })?;
        // rawler0.8 removed raw_to_srgb's cpp==1 assertion, but its linear CropDefault still
        // subtracts an active-area origin that was never cropped by the CFA demosaic stage.
        if raw.cpp == 3 && matches!(raw.photometric, RawPhotometricInterpretation::LinearRaw) {
            bail!(
                "linear RAW viewer development is not supported: its crop path is not implemented"
            );
        }
        normalize_dng_sensor_origin(&mut raw, decoder.format_hint())?;
        normalize_calibration_to_d65(&mut raw)?;
        let intermediate = if is_xtrans(&raw) {
            crate::xtrans::develop_xtrans(&raw, &|| false)?
        } else {
            RawDevelop::default().develop_intermediate(&raw)?
        };
        intermediate
            .to_dynamic_image()
            .context("RAW viewer developer returned invalid dimensions")
    })
}

/// A cancelled RAW export is distinct from an unsupported/corrupt source. No pixels are returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawExportCancelled;

impl std::fmt::Display for RawExportCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RAW export cancelled")
    }
}

impl std::error::Error for RawExportCancelled {}

/// Develop the RAW source to full default-crop, **unrotated sRGB** pixels. `keep.depth` retains
/// RGB16; otherwise return RGB8. RAW has no alpha channel. The caller must use RAW orientation
/// (`read_orientation(shot, true)`), its pending rotation, and `Gamut::Srgb` before export.
/// There is never a finished-sibling or embedded-preview fallback.
///
/// Cancellation is observed before I/O, before and after the rawler decode, after development,
/// and after conversion. Rawler cannot be interrupted inside a decode/develop call. A per-file
/// unwind is converted to an error, allowing a batch caller to continue with the next source;
/// allocation aborts/OS failures are not catchable Rust panics.
pub fn develop_raw_pixels_for_export(
    shot: &Shot,
    keep: Keep,
    cancelled: impl Fn() -> bool,
) -> Result<(Pixels, u32, u32)> {
    contain_panic(|| {
        check_cancel(&cancelled)?;
        if shot.cloud_placeholder {
            bail!("RAW source is not downloaded (cloud placeholder)");
        }
        let path = shot
            .raw
            .as_deref()
            .context("shot has no RAW file to develop")?;
        require_downloaded(path)?;
        let source = RawSource::new(path).context("open RAW for export")?;
        let decoder = rawler::get_decoder(&source).context("identify RAW for export")?;
        let preflight = preflight_dng(decoder.as_ref(), true)?;
        check_cancel(&cancelled)?;
        // Use explicit development: rawler's default X-Trans route is a different quality tier.
        // Do not use dummy=true as a generic allocation guard: some decoders still allocate/decode.
        let mut raw = decoder
            .raw_image(&source, &RawDecodeParams::default(), false)
            .context("decode RAW for export")?;
        if let Some(dims) = preflight {
            if dims != (raw.width, raw.height) {
                bail!("RAW dimensions changed between DNG preflight and decode");
            }
        }
        normalize_dng_sensor_origin(&mut raw, decoder.format_hint())?;
        normalize_calibration_to_d65(&mut raw)?;
        check_cancel(&cancelled)?;
        develop_validated(&raw, keep, &cancelled)
    })
}

// Keep header validation before the callback that can allocate the sensor buffer.
// Non-DNG codecs still need a decoded-size check before the developer expands RGB.
fn load_viewer_raw(decoder: &dyn Decoder, decode: impl FnOnce() -> Result<RawImage>) -> Result<RawImage> {
    let preflight = preflight_dng(decoder, false)?;
    let raw = decode()?;
    guard_source_dims(u32::try_from(raw.width)?, u32::try_from(raw.height)?, "RAW viewer source")?;
    if preflight.is_some_and(|dims| dims != (raw.width, raw.height)) {
        bail!("RAW dimensions changed between DNG preflight and decode");
    }
    Ok(raw)
}

fn contain_panic<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(anyhow::anyhow!(
            "RAW export decoder/developer panicked; no developed pixels returned"
        ))
    })
}

fn check_cancel(cancelled: &impl Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(RawExportCancelled.into())
    } else {
        Ok(())
    }
}

fn require_downloaded(path: &std::path::Path) -> Result<()> {
    if crate::file_is_cloud_placeholder(path) {
        bail!("RAW source is not downloaded (cloud placeholder)");
    }
    Ok(())
}

fn selected_calibration(raw: &RawImage) -> Option<(Illuminant, Vec<f32>)> {
    raw.color_matrix_find_first([
        Illuminant::D65,
        Illuminant::A,
        Illuminant::B,
        Illuminant::C,
        Illuminant::D50,
        Illuminant::D55,
        Illuminant::D75,
        Illuminant::Daylight,
        Illuminant::Flash,
    ])
}

/// A ColorMatrix maps XYZ under its calibration illuminant INTO camera RGB (DNG1.7.1 p33).
/// Its input therefore needs D65→source adaptation: M_source × CAT(D65→source). Rawler0.8's
/// RawDevelop applies the opposite CAT; normalize once before that path can select it.
/// This small metadata helper is shared with explicit X-Trans development; it never clones pixels.
pub(crate) fn calibration_d65(raw: &RawImage) -> Result<[[f32; 3]; 3]> {
    let (illuminant, color) =
        selected_calibration(raw).context("RAW has no supported calibration matrix")?;
    if color.len() != 9 || color.iter().any(|v| !v.is_finite()) {
        bail!("RAW calibration must be a finite 3x3 matrix");
    }
    let matrix = [
        [color[0], color[1], color[2]],
        [color[3], color[4], color[5]],
        [color[6], color[7], color[8]],
    ];
    Ok(if illuminant == Illuminant::D65 {
        matrix
    } else {
        rawler::imgop::chromatic_adaption::adapt_bradford(&Illuminant::D65, &illuminant, &matrix)
    })
}

fn normalize_calibration_to_d65(raw: &mut RawImage) -> Result<()> {
    // Keep existing D65 data (including viewer-only four-plane matrices) entirely unchanged.
    // Missing/unknown calibration remains the viewer's existing fallback; export validates it.
    if raw.color_matrix.contains_key(&Illuminant::D65) || selected_calibration(raw).is_none() {
        return Ok(());
    }
    let matrix = calibration_d65(raw)?;
    raw.color_matrix
        .insert(Illuminant::D65, matrix.into_iter().flatten().collect());
    Ok(())
}

/// DNG's public Raw IFD names the exact dimensions the decoder allocates. Other rawler formats
/// have no common, allocation-free size API: validate them immediately after raw_image instead.
fn preflight_dng(decoder: &dyn Decoder, require_cfa: bool) -> Result<Option<(usize, usize)>> {
    if decoder.format_hint() != FormatHint::DNG {
        return Ok(None);
    }
    let ifd = decoder
        .ifd(WellKnownIFD::Raw)?
        .context("DNG has no RAW IFD")?;
    let scalar = |tag: u16| -> Result<u32> {
        let entry = ifd
            .get_entry(tag)
            .with_context(|| format!("DNG is missing tag {tag}"))?;
        match &entry.value {
            Value::Short(v) if v.len() == 1 => Ok(v[0] as u32),
            Value::Long(v) if v.len() == 1 => Ok(v[0]),
            _ => bail!("DNG tag {tag} is not one unsigned dimension/value"),
        }
    };
    let (w, h) = (scalar(256)?, scalar(257)?);
    guard_source_dims(w, h, "RAW DNG")?;
    if require_cfa && (scalar(277)? != 1 || scalar(262)? != 32803) {
        bail!("RAW export does not yet support linear or monochrome DNG development");
    }
    Ok(Some((w as usize, h as usize)))
}

/// Adobe DNG 1.7.1 (PhotometricInterpretation p22, BlackLevel p28): both repeating patterns
/// originate at ActiveArea's top-left. Rawler 0.8 shifts the CFA forward instead of backward and
/// leaves the black-level tile unshifted. The CFA mistake is invisible at Bayer's period2, but
/// changes X-Trans colors. Normalize once to full-sensor coordinates before either CPU consumer.
/// This must be revisited if the pinned dependency fixes its DNG origin handling.
fn normalize_dng_sensor_origin(raw: &mut RawImage, format: FormatHint) -> Result<()> {
    if format != FormatHint::DNG {
        return Ok(());
    }
    let Some(active) = raw.active_area else {
        return Ok(());
    };
    let RawPhotometricInterpretation::Cfa(config) = &mut raw.photometric else {
        return Ok(());
    };
    if config.cfa.width == 6 && config.cfa.height == 6 {
        // Undo rawler's +origin, then apply the required -origin: net -2*origin modulo6.
        let dx = (6 - 2 * (active.p.x % 6) % 6) % 6;
        let dy = (6 - 2 * (active.p.y % 6) % 6) % 6;
        config.cfa = config.cfa.shift(dx, dy);
        raw.camera.cfa = config.cfa.clone();
    }
    let b = &raw.blacklevel;
    if b.cpp != 1
        || !matches!((b.width, b.height), (1, 1) | (2, 2) | (6, 6))
        || b.levels.len() != b.width * b.height
    {
        bail!("unsupported DNG black-level tile for active-area normalization");
    }
    let dx = (b.width - active.p.x % b.width) % b.width;
    let dy = (b.height - active.p.y % b.height) % b.height;
    raw.blacklevel = b.shift(dx, dy);
    Ok(())
}

fn checked_rect(rect: Rect, width: usize, height: usize, name: &str) -> Result<()> {
    if rect.d.w == 0
        || rect.d.h == 0
        || rect.p.x.checked_add(rect.d.w).is_none_or(|end| end > width)
        || rect
            .p
            .y
            .checked_add(rect.d.h)
            .is_none_or(|end| end > height)
    {
        bail!("invalid RAW {name} crop");
    }
    Ok(())
}

/// Keep this gate separate from the development stage: a future X-Trans developer must validate
/// its own CFA/level assumptions rather than weakening the Bayer PPG preconditions below.
fn validate_bayer(raw: &RawImage) -> Result<(u32, u32)> {
    let (w, h) = (u32::try_from(raw.width)?, u32::try_from(raw.height)?);
    guard_source_dims(w, h, "RAW export source")?;
    let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else {
        bail!(
            "RAW export supports Bayer CFA development; linear/monochrome input is not implemented"
        );
    };
    let cfa = &config.cfa;
    if config.sensor != rawler::imgop::sensor::SensorType::Bayer
        || raw.cpp != 1
        || cfa.width != 2
        || cfa.height != 2
        || !matches!(cfa.to_string().as_str(), "RGGB" | "BGGR" | "GRBG" | "GBRG")
    {
        bail!("unsupported RAW export CFA {}x{} ({cfa}); Bayer PPG cannot develop X-Trans or other layouts", cfa.width, cfa.height);
    }
    // rawler's scaling uses chunks_exact(2 rows), then chunks_exact(2 columns).
    if !raw.width.is_multiple_of(2) || !raw.height.is_multiple_of(2) {
        bail!("RAW Bayer dimensions must be even; rawler would leave an unscaled edge");
    }
    let samples = raw
        .width
        .checked_mul(raw.height)
        .context("RAW sample count overflow")?;
    let len = match &raw.data {
        RawImageData::Integer(v) => v.len(),
        RawImageData::Float(v) => {
            if v.iter().any(|v| !v.is_finite()) {
                bail!("RAW samples contain non-finite values");
            }
            v.len()
        }
    };
    if len != samples {
        bail!("RAW sample buffer length does not match its dimensions");
    }
    let active = raw
        .active_area
        .unwrap_or_else(|| Rect::new(rawler::imgop::Point::zero(), raw.dim()));
    checked_rect(active, raw.width, raw.height, "active-area")?;
    if active.d.w < 6 || active.d.h < 6 {
        bail!("RAW active area is too small for Bayer PPG (minimum 6x6)");
    }
    let crop = raw.crop_area.unwrap_or(active);
    checked_rect(crop, raw.width, raw.height, "default")?;
    if crop.p.x < active.p.x
        || crop.p.y < active.p.y
        || crop.p.x + crop.d.w > active.p.x + active.d.w
        || crop.p.y + crop.d.h > active.p.y + active.d.h
    {
        bail!("RAW default crop is outside its active area");
    }

    let b = &raw.blacklevel;
    if b.cpp != 1
        || !matches!((b.width, b.height), (1, 1) | (2, 2))
        || b.levels.len() != b.width * b.height
        || !matches!(raw.whitelevel.0.len(), 1 | 4)
    {
        bail!("unsupported RAW Bayer black/white level layout");
    }
    let black = b.as_bayer_array();
    let white = raw.whitelevel.as_bayer_array();
    if black
        .iter()
        .zip(white)
        .any(|(&b, w)| !b.is_finite() || !w.is_finite() || b < 0.0 || w <= b)
    {
        bail!("invalid RAW black/white levels");
    }
    if raw.wb_coeffs[..3]
        .iter()
        .any(|v| !v.is_finite() || *v <= 0.0)
    {
        bail!("RAW has no usable as-shot RGB white balance");
    }
    // Same calibration matrix path as rawler, including its padded fourth row. Refuse singular
    // transforms before they become black pixels through float→integer conversion.
    let xyz2cam = calibration_d65(raw)?;
    let xyz2cam = [xyz2cam[0], xyz2cam[1], xyz2cam[2], [0.0; 3]];
    let rgb2cam = matrix::normalize(matrix::multiply(
        &xyz2cam,
        &rawler::imgop::xyz::SRGB_TO_XYZ_D65,
    ));
    if matrix::pseudo_inverse(rgb2cam)
        .iter()
        .flatten()
        .any(|v| !v.is_finite())
    {
        bail!("RAW calibration matrix is singular or non-finite");
    }
    Ok((u32::try_from(crop.d.w)?, u32::try_from(crop.d.h)?))
}

fn develop_validated(
    raw: &RawImage,
    keep: Keep,
    cancelled: &impl Fn() -> bool,
) -> Result<(Pixels, u32, u32)> {
    let (expected, intermediate) = if is_xtrans(raw) {
        // The X-Trans stage owns its independent CFA, crop, level and calibration validation.
        let intermediate = crate::xtrans::develop_xtrans(raw, cancelled)?;
        let crop = raw
            .crop_area
            .or(raw.active_area)
            .unwrap_or_else(|| Rect::new(rawler::imgop::Point::zero(), raw.dim()));
        (
            (u32::try_from(crop.d.w)?, u32::try_from(crop.d.h)?),
            intermediate,
        )
    } else {
        let expected = validate_bayer(raw)?;
        check_cancel(cancelled)?;
        (
            expected,
            RawDevelop::default()
                .develop_intermediate(raw)
                .context("develop RAW for export")?,
        )
    };
    check_cancel(cancelled)?;
    let Intermediate::ThreeColor(ref pixels) = intermediate else {
        bail!("RAW developer did not return RGB");
    };
    if pixels.data.iter().flatten().any(|v| !v.is_finite()) {
        bail!("RAW developer returned non-finite RGB");
    }
    let image = intermediate
        .to_dynamic_image()
        .context("RAW RGB buffer has invalid dimensions")?;
    let dims = (image.width(), image.height());
    if dims != expected {
        bail!("RAW developed dimensions differ from the validated crop");
    }
    let pixels = if keep.depth {
        Pixels::Rgb16(image.into_rgb16().into_raw())
    } else {
        Pixels::Rgb8(image.into_rgb8().into_raw())
    };
    check_cancel(cancelled)?;
    Ok((pixels, dims.0, dims.1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::formats::tiff::{DirectoryWriter, Rational, SRational, TiffWriter};
    use std::cell::Cell;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    // A portable, genuine uncompressed Bayer DNG; no camera database or donated fixture needed.
    // Black levels vary with sensor position. The XYZ→camera matrix makes camera RGB equal to
    // linear sRGB, so constant neutral patches have an independently calculable output.
    fn dng(pattern: &str, orientation: u16, cropped: bool, declared_width: u32) -> Vec<u8> {
        dng_at_origin(pattern, orientation, cropped, declared_width, 2)
    }

    fn dng_at_origin(
        pattern: &str,
        orientation: u16,
        cropped: bool,
        declared_width: u32,
        origin: usize,
    ) -> Vec<u8> {
        dng_variant(
            pattern,
            orientation,
            cropped,
            declared_width,
            origin,
            Illuminant::D65,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dng_variant(
        pattern: &str,
        orientation: u16,
        cropped: bool,
        declared_width: u32,
        origin: usize,
        illuminant: Illuminant,
        linear: bool,
    ) -> Vec<u8> {
        let (w, h) = (24u32, 20u32);
        let cfa = rawler::CFA::new(pattern);
        let mut data = Vec::new();
        for y in 0..h as usize {
            for x in 0..w as usize {
                let phase = (y % 2) * 2 + x % 2;
                let black = [64u16, 128, 192, 256][phase];
                // Neutral center and deliberately colored edges provide phase/crop falsifiers.
                let signal = if (5..19).contains(&x) && (5..15).contains(&y) {
                    0.125
                } else {
                    [0.25, 0.125, 0.0625][cfa.color_at(y, x)]
                };
                if linear {
                    for channel in 0..3 {
                        data.push(300 + 100 * channel + (x + 10 * y) as u16);
                    }
                } else {
                    data.push(black + ((4095 - black) as f32 * signal).round() as u16);
                }
            }
        }
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut cursor).unwrap();
        let offset = writer.write_data_u16_le(&data).unwrap();
        let mut tags = DirectoryWriter::new();
        tags.add_tag(256u16, declared_width);
        tags.add_tag(257u16, h);
        tags.add_tag(258u16, 16u16);
        tags.add_tag(259u16, 1u16);
        tags.add_tag(262u16, if linear { 34892u16 } else { 32803u16 });
        tags.add_tag(271u16, "Falcon synthetic");
        tags.add_tag(272u16, "Portable Bayer export fixture");
        tags.add_tag(273u16, offset);
        tags.add_tag(274u16, orientation);
        tags.add_tag(277u16, if linear { 3u16 } else { 1u16 });
        tags.add_tag(278u16, h);
        tags.add_tag(279u16, (data.len() * 2) as u32);
        tags.add_tag(33421u16, [cfa.height as u16, cfa.width as u16]);
        // DNG 1.7.1 PhotometricInterpretation: the CFA tag begins at ActiveArea, not sensor(0,0).
        let stored_cfa = if cropped {
            cfa.shift(origin, origin)
        } else {
            cfa.clone()
        };
        tags.add_tag(33422u16, Value::Byte(stored_cfa.flat_pattern()));
        tags.add_tag(50706u16, [1u8, 4, 0, 0]);
        tags.add_tag(50707u16, [1u8, 1, 0, 0]);
        tags.add_tag(50708u16, "Falcon portable Bayer");
        tags.add_tag(50713u16, if linear { [1u16, 1] } else { [2u16, 2] });
        let sensor_black = rawler::rawimage::BlackLevel::new(&[64u16, 128, 192, 256], 2, 2, 1);
        let stored_black = if cropped {
            sensor_black.shift(origin, origin)
        } else {
            sensor_black
        };
        tags.add_tag(
            50714u16,
            Value::Rational(if linear {
                vec![Rational::new(0, 1); 3]
            } else {
                stored_black.levels
            }),
        );
        tags.add_tag(50717u16, 4095u32);
        let inv = matrix::pseudo_inverse(rawler::imgop::xyz::SRGB_TO_XYZ_D65);
        // Equivalent A-calibrated camera: M_A = M_D65 × CAT(A→D65), with camera white still[1,1,1].
        let inv = if illuminant == Illuminant::D65 {
            inv
        } else {
            matrix::multiply(
                &inv,
                &rawler::imgop::chromatic_adaption::bradford_adaption_matrix(
                    &illuminant,
                    &Illuminant::D65,
                ),
            )
        };
        let matrix: Vec<SRational> = inv
            .iter()
            .flatten()
            .map(|v| SRational::new((v * 1_000_000.0).round() as i32, 1_000_000))
            .collect();
        tags.add_tag(50721u16, Value::SRational(matrix));
        tags.add_tag(50728u16, Value::Rational(vec![Rational::new(1, 1); 3]));
        tags.add_tag(50778u16, illuminant as u16);
        if cropped {
            tags.add_tag(50829u16, [origin as u32, origin as u32, 18, 22]); // top,left,bottom,right
            tags.add_tag(50719u16, [1u32, 1]); // relative to active origin: odd sensor phase
            tags.add_tag(50720u16, [16u32, 12]);
        }
        writer.build(tags).unwrap();
        cursor.into_inner()
    }

    struct Fixture {
        path: PathBuf,
    }
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "falcon_raw_export_{}_{}.dng",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, bytes).unwrap();
            Self { path }
        }
        fn shot(&self) -> Shot {
            Shot {
                id: 0,
                name: "portable".into(),
                has_raw: true,
                has_jpg: false,
                raw: Some(self.path.clone()),
                jpg: None,
                kind: crate::SrcKind::Jpeg,
                cloud_placeholder: false,
                sniffed: None,
            }
        }
        fn raw(&self) -> RawImage {
            rawler::analyze::extract_raw_pixels(&self.path, &RawDecodeParams::default()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn raw_export_bayer_phases_preserve_precision_color_crop_and_unrotated_contract() {
        for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let fixture = Fixture::new(&dng(pattern, 6, true, 24));
            let shot = fixture.shot();
            let (px, w, h) = develop_raw_pixels_for_export(&shot, Keep::ALL, || false).unwrap();
            assert_eq!(
                (w, h),
                (16, 12),
                "full default crop; orientation must remain unapplied"
            );
            assert_eq!(crate::read_orientation(&shot, true), Some(6));
            let Pixels::Rgb16(v) = px else {
                panic!("RAW PNG must retain16-bit")
            };
            assert!(
                v.iter().any(|v| v % 257 != 0),
                "actual sub-8-bit precision survived"
            );
            let center = &v[(6 * w as usize + 8) * 3..][..3];
            let expected = (1.055 * 0.125f64.powf(1.0 / 2.4) - 0.055) * 65535.0;
            for value in center {
                assert!(
                    (*value as f64 - expected).abs() < 70.0,
                    "neutral sRGB {center:?} expected {expected}"
                );
            }
            let (px8, w8, h8) = develop_raw_pixels_for_export(&shot, Keep::NONE, || false).unwrap();
            assert_eq!((w8, h8), (w, h));
            let Pixels::Rgb8(v8) = px8 else {
                panic!("RAW JPEG is RGB8")
            };
            assert_eq!(v8.len(), v.len());
            assert!((v8[(6 * w as usize + 8) * 3] as f64 - expected / 257.0).abs() <= 1.0);

            let upright = Fixture::new(&dng(pattern, 1, true, 24));
            assert_eq!(
                develop_raw_pixels_for_export(&upright.shot(), Keep::ALL, || false)
                    .unwrap()
                    .0,
                Pixels::Rgb16(v)
            );
        }
    }

    #[test]
    fn raw_export_crop_is_the_sensor_default_crop_not_a_resized_preview() {
        let full = Fixture::new(&dng("RGGB", 1, false, 24));
        let crop = Fixture::new(&dng("RGGB", 1, true, 24));
        let (Pixels::Rgb16(full), fw, _) =
            develop_raw_pixels_for_export(&full.shot(), Keep::ALL, || false).unwrap()
        else {
            panic!()
        };
        let (Pixels::Rgb16(crop), cw, _) =
            develop_raw_pixels_for_export(&crop.shot(), Keep::ALL, || false).unwrap()
        else {
            panic!()
        };
        // Interior is far from either active-area interpolation boundary. Crop origin is (3,3).
        for y in 4..8usize {
            for x in 4..12usize {
                let a = ((y + 3) * fw as usize + x + 3) * 3;
                let b = (y * cw as usize + x) * 3;
                assert_eq!(&full[a..a + 3], &crop[b..b + 3]);
            }
        }
    }

    #[test]
    fn raw_export_xtrans_dng_active_origin_has_correct_sensor_phase_in_export_and_viewer() {
        let pattern = "GBGGRGRGRBGBGBGGRGGRGGBGBGBRGRGRGGBG";
        let bytes = dng(pattern, 6, true, 24);
        let fixture = Fixture::new(&bytes);
        // Independent oracle: the fixture's samples were authored in the stated sensor CFA.
        // The RAW header stores that pattern relative to ActiveArea(2,2), per the Adobe spec.
        let mut expected_raw = fixture.raw();
        if let RawPhotometricInterpretation::Cfa(ref mut config) = expected_raw.photometric {
            config.cfa = rawler::CFA::new(pattern);
        }
        expected_raw.camera.cfa = rawler::CFA::new(pattern);
        let expected = crate::xtrans::develop_xtrans(&expected_raw, &|| false)
            .unwrap()
            .to_dynamic_image()
            .unwrap()
            .into_rgb16()
            .into_raw();
        let (pixels, w, h) =
            develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || false).unwrap();
        assert_eq!((w, h), (16, 12));
        assert_eq!(
            pixels,
            Pixels::Rgb16(expected.clone()),
            "X-Trans DNG must undo ActiveArea phase, not apply it twice"
        );
        let viewed = develop_raw_image_for_viewer(&fixture.path).unwrap();
        assert_eq!(
            (viewed.width(), viewed.height()),
            (16, 12),
            "metadata orientation remains unapplied"
        );
        assert_eq!(
            viewed.into_rgb16().into_raw(),
            expected,
            "CPU viewer shares the corrected DNG source phase"
        );
        assert_eq!(
            std::fs::read(&fixture.path).unwrap(),
            bytes,
            "development never changes the source"
        );
    }

    #[test]
    fn raw_export_bayer_dng_active_origin_has_correct_black_levels_in_export_and_viewer() {
        let fixture = Fixture::new(&dng_at_origin("RGGB", 1, true, 24, 1));
        let mut expected_raw = fixture.raw();
        // Samples use this full-sensor tile. DNG BlackLevel stores it relative to ActiveArea(1,1).
        expected_raw.blacklevel =
            rawler::rawimage::BlackLevel::new(&[64u16, 128, 192, 256], 2, 2, 1);
        let expected = RawDevelop::default()
            .develop_intermediate(&expected_raw)
            .unwrap()
            .to_dynamic_image()
            .unwrap()
            .into_rgb16()
            .into_raw();
        let (pixels, w, h) =
            develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || false).unwrap();
        assert_eq!((w, h), (16, 12));
        assert_eq!(
            pixels,
            Pixels::Rgb16(expected.clone()),
            "DNG black-level tile starts at ActiveArea, not full-sensor origin"
        );
        assert_eq!(
            develop_raw_image_for_viewer(&fixture.path)
                .unwrap()
                .into_rgb16()
                .into_raw(),
            expected
        );
    }

    #[test]
    fn raw_export_developed_png_roundtrips_srgb_at_sixteen_bits() {
        let fixture = Fixture::new(&dng("RGGB", 1, true, 24));
        let (pixels, w, h) =
            develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || false).unwrap();
        let expected = pixels.clone();
        let output = Fixture::new(&[]); // Own scratch file; its RAII cleanup also covers assertion failures.
        crate::export_web_file(
            pixels,
            w,
            h,
            &crate::WebSpec {
                long: u32::MAX,
                quality: 90,
                wm: None,
                src: falcon_color::Gamut::Srgb,
                fmt: crate::WebFormat::Png,
            },
            &output.path,
        )
        .unwrap();
        let mut reader = png::Decoder::new(std::fs::File::open(&output.path).unwrap())
            .read_info()
            .unwrap();
        assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
        assert_eq!(reader.info().color_type, png::ColorType::Rgb);
        assert!(reader.info().srgb.is_some());
        let mut bytes = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut bytes).unwrap();
        assert_eq!(
            (info.width, info.height),
            (w, h),
            "Full export never upscales"
        );
        let samples = bytes[..info.buffer_size()]
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(Pixels::Rgb16(samples), expected);
    }

    #[test]
    fn raw_export_non_d65_calibration_matches_equivalent_d65_camera_in_export_and_viewer() {
        let d65 = Fixture::new(&dng_variant(
            "RGGB",
            1,
            false,
            24,
            2,
            Illuminant::D65,
            false,
        ));
        let a = Fixture::new(&dng_variant("RGGB", 1, false, 24, 2, Illuminant::A, false));
        let (Pixels::Rgb16(expected), _, _) =
            develop_raw_pixels_for_export(&d65.shot(), Keep::ALL, || false).unwrap()
        else {
            panic!()
        };
        let (Pixels::Rgb16(actual), _, _) =
            develop_raw_pixels_for_export(&a.shot(), Keep::ALL, || false).unwrap()
        else {
            panic!()
        };
        let max_delta = expected
            .iter()
            .zip(&actual)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            max_delta <= 8,
            "equivalent A/D65 cameras must not change colors: max16-bit delta{max_delta}"
        );
        let viewed = develop_raw_image_for_viewer(&a.path)
            .unwrap()
            .into_rgb16()
            .into_raw();
        assert_eq!(
            viewed, actual,
            "viewer and export use the same calibrated RAW colors"
        );
    }

    #[test]
    fn raw_export_viewer_refuses_linear_dng_before_the_known_default_crop_error() {
        let fixture = Fixture::new(&dng_variant("RGGB", 1, true, 24, 2, Illuminant::D65, true));
        let raw = fixture.raw();
        assert_eq!(raw.cpp, 3);
        assert!(matches!(
            raw.photometric,
            RawPhotometricInterpretation::LinearRaw
        ));
        let error = develop_raw_image_for_viewer(&fixture.path)
            .expect_err("linear DNG has no corrected viewer crop path yet");
        assert!(error.to_string().contains("linear"), "{error:#}");
        assert!(develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || false).is_err());
    }

    #[test]
    fn raw_export_cloud_flag_refuses_before_any_decoder_reads() {
        let fixture = Fixture::new(b"invalid local bytes must never reach a decoder");
        let mut shot = fixture.shot();
        shot.cloud_placeholder = true;
        let error = develop_raw_pixels_for_export(&shot, Keep::ALL, || false).unwrap_err();
        assert!(
            error.to_string().contains("not downloaded"),
            "cloud refusal must precede decoder identification: {error:#}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn raw_export_windows_offline_attribute_refuses_export_and_viewer_before_decode() {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileAttributesW(path: *const u16) -> u32;
            fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32;
        }
        struct Restore {
            path: Vec<u16>,
            attributes: u32,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    SetFileAttributesW(self.path.as_ptr(), self.attributes);
                }
            }
        }
        let fixture = Fixture::new(b"invalid local bytes; only metadata may be read");
        let path: Vec<u16> = fixture
            .path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let attrs = unsafe { GetFileAttributesW(path.as_ptr()) };
        assert_ne!(attrs, u32::MAX);
        let restore = Restore {
            path,
            attributes: attrs,
        };
        assert_ne!(
            unsafe {
                SetFileAttributesW(restore.path.as_ptr(), attrs | crate::FILE_ATTRIBUTE_OFFLINE)
            },
            0
        );
        assert!(crate::file_is_cloud_placeholder(&fixture.path));
        let export =
            develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || false).unwrap_err();
        let viewer = develop_raw_image_for_viewer(&fixture.path)
            .expect_err("viewer must refuse offline RAW");
        assert!(export.to_string().contains("not downloaded"), "{export:#}");
        assert!(viewer.to_string().contains("not downloaded"), "{viewer:#}");
        drop(restore);
        assert!(!crate::file_is_cloud_placeholder(&fixture.path));
    }

    #[test]
    fn raw_export_cancellation_at_every_boundary_returns_no_pixels() {
        let fixture = Fixture::new(&dng("RGGB", 1, true, 24));
        for stop in 1..=6 {
            let count = Cell::new(0);
            let err = develop_raw_pixels_for_export(&fixture.shot(), Keep::ALL, || {
                count.set(count.get() + 1);
                count.get() == stop
            })
            .unwrap_err();
            assert!(err.is::<RawExportCancelled>(), "checkpoint{stop}: {err:#}");
        }
        let missing = Shot {
            raw: None,
            ..fixture.shot()
        };
        assert!(
            develop_raw_pixels_for_export(&missing, Keep::ALL, || true)
                .unwrap_err()
                .is::<RawExportCancelled>(),
            "cancel before any path access"
        );
    }

    // Falsifier: call decode before DNG preflight. The callback marks the allocation
    // boundary; this test never attempts the oversized rawler allocation itself.
    #[test]
    fn refinement_viewer_raw_rejects_large_dng_before_decode() {
        let bomb=Fixture::new(&dng("RGGB",1,false,u32::MAX));
        let source=RawSource::new(&bomb.path).unwrap();
        let decoder=rawler::get_decoder(&source).unwrap();
        let reached=std::cell::Cell::new(false);
        let err=load_viewer_raw(decoder.as_ref(),|| {
            reached.set(true); bail!("pixel allocation boundary reached")
        }).unwrap_err();
        assert!(!reached.get(),"viewer reached pixel allocation before checking declared dimensions");
        assert!(err.to_string().contains("RAW DNG too large"),"{err:#}");
    }

    // Falsifier: remove the post-decode limit. A small fixture's synthetic metadata
    // can then promise a huge development buffer even though no huge pixels exist here.
    #[test]
    fn refinement_viewer_raw_checks_decoded_size_before_development() {
        let fixture=Fixture::new(&dng("RGGB",1,false,24));
        let source=RawSource::new(&fixture.path).unwrap();
        let decoder=rawler::get_decoder(&source).unwrap();
        let mut raw=fixture.raw(); raw.width=u32::MAX as usize;
        let err=load_viewer_raw(decoder.as_ref(),||Ok(raw)).unwrap_err();
        assert!(err.to_string().contains("RAW viewer source too large"),"{err:#}");
    }

    #[test]
    fn raw_export_dng_dimension_preflight_precedes_pixel_allocation() {
        let bomb = Fixture::new(&dng("RGGB", 1, false, u32::MAX));
        let err = develop_raw_pixels_for_export(&bomb.shot(), Keep::ALL, || false).unwrap_err();
        assert!(err.to_string().contains("RAW DNG too large"), "{err:#}");
    }

    #[test]
    fn raw_export_invalid_metadata_is_refused_before_development() {
        let fixture = Fixture::new(&dng("RGGB", 1, false, 24));
        let original = fixture.raw();
        let check = |raw: &RawImage, message: &str| {
            let err = validate_bayer(raw).unwrap_err();
            assert!(
                err.to_string().contains(message),
                "expected{message}: {err:#}"
            );
        };
        let mut raw = original.clone();
        raw.width = 23;
        check(&raw, "even");
        let mut raw = original.clone();
        raw.height = 0;
        check(&raw, "empty");
        let mut raw = original.clone();
        raw.data = RawImageData::Integer(vec![0]);
        check(&raw, "buffer length");
        let mut raw = original.clone();
        raw.data = RawImageData::Float(vec![f32::NAN; 480]);
        check(&raw, "non-finite");
        let mut raw = original.clone();
        raw.wb_coeffs[1] = 0.0;
        check(&raw, "white balance");
        let mut raw = original.clone();
        raw.color_matrix.clear();
        check(&raw, "calibration matrix");
        let mut raw = original.clone();
        raw.color_matrix.insert(Illuminant::D65, vec![0.0; 9]);
        check(&raw, "singular");
        let mut raw = original.clone();
        raw.whitelevel.0 = vec![1];
        check(&raw, "black/white");
        let mut raw = original.clone();
        raw.crop_area = Some(Rect::new(
            rawler::imgop::Point::new(23, 0),
            rawler::imgop::Dim2::new(5, 5),
        ));
        check(&raw, "default");
        let mut raw = original.clone();
        raw.active_area = Some(Rect::new(
            rawler::imgop::Point::zero(),
            rawler::imgop::Dim2::new(4, 4),
        ));
        check(&raw, "too small");
        let mut raw = original.clone();
        raw.photometric = RawPhotometricInterpretation::LinearRaw;
        raw.cpp = 3;
        check(&raw, "linear/monochrome");
        let mut raw = original;
        if let RawPhotometricInterpretation::Cfa(ref mut c) = raw.photometric {
            c.cfa = rawler::CFA::new("GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG");
        }
        check(&raw, "X-Trans");
    }

    #[test]
    fn raw_export_error_and_panic_do_not_poison_the_next_file() {
        let bad = Fixture::new(b"this is not a RAW");
        assert!(develop_raw_pixels_for_export(&bad.shot(), Keep::ALL, || false).is_err());
        let good = Fixture::new(&dng("RGGB", 1, false, 24));
        let checks = Cell::new(0);
        let panic_result = develop_raw_pixels_for_export(&good.shot(), Keep::ALL, || {
            checks.set(checks.get() + 1);
            // Inject the unwind through the public boundary after a real successful decode.
            assert_ne!(checks.get(), 4, "simulated worker-stage panic");
            false
        });
        assert!(panic_result.unwrap_err().to_string().contains("panicked"));
        assert_eq!(
            develop_raw_pixels_for_export(&good.shot(), Keep::ALL, || false)
                .unwrap()
                .1,
            24
        );
    }
}
