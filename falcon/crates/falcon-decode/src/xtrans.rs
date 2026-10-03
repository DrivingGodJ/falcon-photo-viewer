//! Shared CPU X-Trans development for the viewer and manufactured exports.
//!
//! Rawler 0.8's default developer still selects bilinear for X-Trans. Call its
//! public Markesteijn implementation explicitly; retain float RGB until the caller
//! chooses RGB8/RGB16. Calibration uses rawler's public matrix, highlight and sRGB
//! helpers. No camera JPEG is a fallback and orientation is left to the caller.

use anyhow::{bail, Context, Result};
use rawler::imgop::develop::Intermediate;
use rawler::imgop::sensor::xtrans::markesteijn::XTransMarkesteijnDemosaic;
use rawler::imgop::sensor::{Demosaic, SensorType};
use rawler::imgop::xyz::SRGB_TO_XYZ_D65;
use rawler::imgop::{matrix, raw::clip_euclidean_norm_avg, srgb::srgb_apply_gamma_n, Point, Rect};
use rawler::pixarray::PixF32;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::{RawImage, RawImageData, CFA};

fn check_cancel(cancelled: &impl Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(crate::RawExportCancelled.into())
    } else {
        Ok(())
    }
}

fn checked_rect(rect: Rect, width: usize, height: usize) -> Result<()> {
    if rect.d.w == 0
        || rect.d.h == 0
        || rect.p.x.checked_add(rect.d.w).is_none_or(|end| end > width)
        || rect
            .p
            .y
            .checked_add(rect.d.h)
            .is_none_or(|end| end > height)
    {
        bail!("X-Trans crop is empty or outside the sensor");
    }
    Ok(())
}

// Markesteijn's hex-neighbor construction assumes the Fujifilm topology, not
// merely any 6x6 array containing RGB. Accept its translations and transpose.
// Red/blue exchange is itself a three-column translation of this pattern.
const XTRANS: &str = "GBGGRGRGRBGBGBGGRGGRGGBGBGBRGRGRGGBG";

fn supported_cfa(cfa: &CFA) -> bool {
    if cfa.width != 6 || cfa.height != 6 {
        return false;
    }
    let canonical = XTRANS.as_bytes();
    [false, true].into_iter().any(|transpose| {
        (0..6).any(|dy| {
            (0..6).any(|dx| {
                (0..6).all(|y| {
                    (0..6).all(|x| {
                        let (row, col) = if transpose { (x, y) } else { (y, x) };
                        let color = match canonical[((row + dy) % 6) * 6 + (col + dx) % 6] {
                            b'R' => 0,
                            b'G' => 1,
                            _ => 2,
                        };
                        cfa.color_at(y, x) == color
                    })
                })
            })
        })
    })
}

fn camera_to_srgb(raw: &RawImage) -> Result<[[f32; 3]; 3]> {
    // The same validated XYZ-D65 -> camera matrix feeds Bayer and X-Trans.
    // Keep the illuminant-direction correction in one shared implementation.
    let xyz_to_camera = crate::raw_export::calibration_d65(raw)?;
    let padded = [
        xyz_to_camera[0],
        xyz_to_camera[1],
        xyz_to_camera[2],
        [0.0; 3],
    ];
    let rgb_to_camera = matrix::normalize(matrix::multiply(&padded, &SRGB_TO_XYZ_D65));
    let inverse = matrix::pseudo_inverse(rgb_to_camera);
    if inverse.iter().flatten().any(|v| !v.is_finite()) {
        bail!("X-Trans calibration matrix is singular or non-finite");
    }
    Ok(inverse.map(|row| [row[0], row[1], row[2]]))
}

/// Full default-crop, unrotated sRGB float pixels. Cancellation is checked between
/// stages and by row while scaling. Rawler's Markesteijn/color parallel calls do
/// not expose interruption hooks; cancellation during them discards the result.
pub(crate) fn develop_xtrans(
    raw: &RawImage,
    cancelled: &impl Fn() -> bool,
) -> Result<Intermediate> {
    check_cancel(cancelled)?;
    crate::guard_source_dims(
        u32::try_from(raw.width)?,
        u32::try_from(raw.height)?,
        "X-Trans source",
    )?;
    let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else {
        bail!("X-Trans development requires a CFA source");
    };
    if raw.cpp != 1 || config.sensor != SensorType::Xtrans || !supported_cfa(&config.cfa) {
        bail!("unsupported X-Trans CFA topology");
    }
    if raw.fuji_rotation_width.is_some() {
        bail!("a diagonal Fuji sensor is not an X-Trans sensor");
    }
    let count = raw
        .width
        .checked_mul(raw.height)
        .context("X-Trans sample count overflow")?;
    let length = match &raw.data {
        RawImageData::Integer(v) => v.len(),
        RawImageData::Float(v) => v.len(),
    };
    if length != count {
        bail!("X-Trans sample buffer does not match its dimensions");
    }
    let active = raw
        .active_area
        .unwrap_or_else(|| Rect::new(Point::zero(), raw.dim()));
    checked_rect(active, raw.width, raw.height)?;
    // Upstream's neighborhood bounds subtract two. A minimum of 6 also ensures
    // every channel is represented at the border of this six-period sensor.
    if active.d.w < 6 || active.d.h < 6 {
        bail!("X-Trans active area is smaller than 6x6");
    }
    let crop = raw.crop_area.unwrap_or(active);
    checked_rect(crop, raw.width, raw.height)?;
    if crop.p.x < active.p.x
        || crop.p.y < active.p.y
        || crop.p.x + crop.d.w > active.p.x + active.d.w
        || crop.p.y + crop.d.h > active.p.y + active.d.h
    {
        bail!("X-Trans default crop lies outside the active area");
    }
    let black = &raw.blacklevel;
    if black.cpp != 1
        || !matches!((black.width, black.height), (1, 1) | (2, 2) | (6, 6))
        || black.levels.len() != black.width * black.height
        || !matches!(raw.whitelevel.0.len(), 1 | 4)
    {
        bail!("unsupported X-Trans black/white level layout");
    }
    let black_values = black.as_vec();
    let white = raw.whitelevel.as_bayer_array();
    for y in 0..6 {
        for x in 0..6 {
            let b = black_values[(y % black.height) * black.width + x % black.width];
            let w = white[(y % 2) * 2 + x % 2];
            if !b.is_finite() || b < 0.0 || !w.is_finite() || w <= b {
                bail!("invalid X-Trans black/white levels");
            }
        }
    }
    if raw.wb_coeffs[..3]
        .iter()
        .any(|v| !v.is_finite() || *v <= 0.0)
    {
        bail!("X-Trans has no usable as-shot RGB white balance");
    }
    let matrix = camera_to_srgb(raw)?;
    check_cancel(cancelled)?;

    // RawImage::apply_scaling treats every CFA as 2x2. Handle the actual black
    // repeat here, including odd sensor sizes, without cloning the decoded RAW.
    // Keep highlight values above white; rawler's color-stage highlight rule is
    // applied after camera WB/calibration, just as in its normal developer.
    let mut scaled = Vec::with_capacity(count);
    for y in 0..raw.height {
        check_cancel(cancelled)?;
        for x in 0..raw.width {
            let i = y * raw.width + x;
            let value = match &raw.data {
                RawImageData::Integer(v) => v[i] as f32,
                RawImageData::Float(v) => v[i],
            };
            if !value.is_finite() {
                bail!("X-Trans samples contain non-finite values");
            }
            let b = black_values[(y % black.height) * black.width + x % black.width];
            let w = white[(y % 2) * 2 + x % 2];
            scaled.push((value - b).max(0.0) / (w - b));
        }
    }
    check_cancel(cancelled)?;
    let mosaic = PixF32::new_with(scaled, raw.width, raw.height);
    let mut rgb = XTransMarkesteijnDemosaic::new_pass_3().demosaic(
        &mosaic,
        &config.cfa,
        &config.colors,
        active,
    );
    drop(mosaic);
    check_cancel(cancelled)?;
    let relative_crop = Rect::new(
        Point::new(crop.p.x - active.p.x, crop.p.y - active.p.y),
        crop.d,
    );
    if relative_crop.p.x != 0 || relative_crop.p.y != 0 || relative_crop.d != rgb.dim() {
        rgb = rgb.crop(relative_crop);
    }
    check_cancel(cancelled)?;
    // Crop commutes with this per-pixel color transform; doing it first avoids
    // allocating/color-transforming pixels outside the deliverable's crop.
    let wb = raw.wb_coeffs;
    rgb.for_each(|pixel| {
        let balanced = std::array::from_fn(|i| pixel[i] * wb[i]);
        let linear = matrix::multiply_row1(&matrix, &balanced);
        if linear.iter().any(|v| !v.is_finite()) {
            return [f32::NAN; 3];
        }
        srgb_apply_gamma_n(clip_euclidean_norm_avg(&linear))
    });
    if rgb.data.iter().flatten().any(|v| !v.is_finite()) {
        bail!("X-Trans developer returned non-finite RGB");
    }
    check_cancel(cancelled)?;
    Ok(Intermediate::ThreeColor(rgb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::decoders::RawDecodeParams;
    use rawler::formats::tiff::{DirectoryWriter, Rational, SRational, TiffWriter, Value};
    use rawler::imgop::xyz::Illuminant;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    // A real DNG container with independently authored sensor samples. The
    // camera matrix is inverse(sRGB->XYZ), so camera channels are linear sRGB.
    // Positional six-period black levels expose accidental Bayer scaling.
    fn fixture(cfa: &CFA, ramp: bool, cropped: bool) -> (Fixture, RawImage) {
        let (width, height) = (181u32, 157u32);
        let mut samples = Vec::new();
        for y in 0..height as usize {
            for x in 0..width as usize {
                let black = 512 + ((y % 6) * 6 + x % 6) as u16 * 7;
                let channel = cfa.color_at(y, x);
                let signal = if ramp {
                    0.1 + x as f32 * 0.001 + y as f32 * 0.0005
                } else {
                    [0.125, 0.25, 0.375][channel]
                };
                samples.push(black + ((16383 - black) as f32 * signal).round() as u16);
            }
        }
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = TiffWriter::new(&mut cursor).unwrap();
        let offset = writer.write_data_u16_le(&samples).unwrap();
        let mut tags = DirectoryWriter::new();
        tags.add_tag(256u16, width);
        tags.add_tag(257u16, height);
        tags.add_tag(258u16, 16u16);
        tags.add_tag(259u16, 1u16);
        tags.add_tag(262u16, 32803u16);
        tags.add_tag(273u16, offset);
        tags.add_tag(277u16, 1u16);
        tags.add_tag(278u16, height);
        tags.add_tag(279u16, width * height * 2);
        tags.add_tag(284u16, 1u16);
        tags.add_tag(274u16, 6u16); // orientation must remain unapplied
        tags.add_tag(271u16, "Falcon");
        tags.add_tag(272u16, "Synthetic X-Trans");
        tags.add_tag(33421u16, [6u16, 6]);
        tags.add_tag(33422u16, Value::Byte(cfa.flat_pattern()));
        tags.add_tag(50706u16, [1u8, 4, 0, 0]);
        tags.add_tag(50707u16, [1u8, 3, 0, 0]);
        tags.add_tag(50708u16, "Falcon synthetic X-Trans");
        tags.add_tag(50713u16, [6u16, 6]);
        tags.add_tag(
            50714u16,
            Value::Long((0..36u32).map(|i| 512 + i * 7).collect()),
        );
        tags.add_tag(50717u16, 16383u32);
        let inverse = matrix::pseudo_inverse(SRGB_TO_XYZ_D65);
        let values: Vec<SRational> = inverse
            .iter()
            .flatten()
            .map(|v| SRational::new((v * 1_000_000.0).round() as i32, 1_000_000))
            .collect();
        tags.add_tag(50721u16, Value::SRational(values));
        tags.add_tag(50778u16, 21u16);
        tags.add_tag(50728u16, Value::Rational(vec![Rational::new(1, 1); 3]));
        writer.build(tags).unwrap();
        let path = std::env::temp_dir().join(format!(
            "falcon_xtrans_{}_{}.dng",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, cursor.into_inner()).unwrap();
        let fixture = Fixture(path);
        let mut raw =
            rawler::analyze::extract_raw_pixels(&fixture.0, &RawDecodeParams::default()).unwrap();
        // This module takes a decoded RawImage whose CFA starts at sensor (0,0).
        // Assign that input contract directly; DNG's ActiveArea-relative CFAPattern
        // conversion is a separate decoder-boundary regression in raw_export.
        if cropped {
            raw.active_area = Some(Rect::new(
                Point::new(5, 7),
                rawler::imgop::Dim2::new(171, 143),
            ));
            raw.crop_area = Some(Rect::new(
                Point::new(12, 16),
                rawler::imgop::Dim2::new(121, 97),
            ));
        }
        (fixture, raw)
    }

    fn rgb(intermediate: Intermediate) -> rawler::pixarray::RgbF32 {
        let Intermediate::ThreeColor(pixels) = intermediate else {
            panic!("expected RGB")
        };
        pixels
    }

    fn expected_srgb(linear: f32) -> f32 {
        // Independent IEC sRGB expression; samples used here exceed the toe.
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }

    #[test]
    fn xtrans_all_cfa_phases_preserve_flat_color_and_sensor_samples() {
        for dy in 0..6 {
            for dx in 0..6 {
                let cfa = CFA::new(XTRANS).shift(dx, dy);
                let (_fixture, raw) = fixture(&cfa, false, true);
                let pixels = rgb(develop_xtrans(&raw, &|| false).unwrap());
                assert_eq!(
                    (pixels.width, pixels.height),
                    (121, 97),
                    "default crop, unrotated"
                );
                for (index, pixel) in pixels.data.iter().enumerate() {
                    for (channel, expected) in [0.125, 0.25, 0.375]
                        .map(expected_srgb)
                        .into_iter()
                        .enumerate()
                    {
                        assert!(
                            (pixel[channel] - expected).abs() < 0.003,
                            "phase {dx},{dy}, pixel {},{}, channel {channel}: {} vs {expected}",
                            index % pixels.width,
                            index / pixels.width,
                            pixel[channel]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn xtrans_smooth_ramp_has_no_tile_seams_and_crops_at_the_sensor_origin() {
        let (_fixture, raw) = fixture(&CFA::new(XTRANS).shift(2, 3), true, true);
        let pixels = rgb(develop_xtrans(&raw, &|| false).unwrap());
        for y in 0..pixels.height {
            for x in 0..pixels.width {
                // Default crop begins at sensor (12,16), not active-relative (7,9).
                let expected =
                    expected_srgb(0.1 + (x + 12) as f32 * 0.001 + (y + 16) as f32 * 0.0005);
                for value in pixels.data[y * pixels.width + x] {
                    assert!(
                        (value - expected).abs() < 0.009,
                        "ramp {x},{y}: {value} vs {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn xtrans_applies_as_shot_white_balance_and_retains_more_than_eight_bits() {
        let (_fixture, mut raw) = fixture(&CFA::new(XTRANS), false, false);
        raw.wb_coeffs = [2.0, 1.0, 0.5, f32::NAN];
        let intermediate = develop_xtrans(&raw, &|| false).unwrap();
        let pixel = rgb(intermediate.clone()).data[80 * raw.width + 80];
        for (got, expected) in pixel
            .into_iter()
            .zip([0.25, 0.25, 0.1875].map(expected_srgb))
        {
            assert!((got - expected).abs() < 0.003, "WB: {got} vs {expected}");
        }
        let image = intermediate.to_dynamic_image().unwrap().into_rgb16();
        assert!(
            image.as_raw().iter().any(|v| !v.is_multiple_of(257)),
            "real 16-bit precision"
        );
    }

    #[test]
    fn xtrans_non_d65_calibration_preserves_an_authored_color() {
        let (_fixture, mut raw) = fixture(&CFA::new(XTRANS).shift(3, 4), false, true);
        // An independently authored A-illuminant camera: inverse(sRGB->XYZ) *
        // Bradford(A->D65), calculated outside the implementation. A white maps
        // to equal camera channels, so unit as-shot WB is physically consistent.
        // The sensor samples still encode known linear sRGB [.125,.25,.375].
        let authored = [
            [2.907_413_5, -2.012_060_6, -0.510_701_2],
            [-1.071_737_8, 2.180_038_5, -0.007_685_34],
            [0.159_296_24, -0.374_450_8, 3.370_700_8],
        ];
        let white = matrix::multiply_row1(&authored, &[1.098_5, 1.0, 0.355_85]);
        assert!(white.iter().all(|v| (*v - 1.0).abs() < 0.000_01));
        raw.color_matrix.clear();
        raw.color_matrix
            .insert(Illuminant::A, authored.into_iter().flatten().collect());
        let pixels = rgb(develop_xtrans(&raw, &|| false).unwrap());
        for pixel in &pixels.data {
            for (got, expected) in pixel.iter().zip([0.125, 0.25, 0.375].map(expected_srgb)) {
                assert!(
                    (got - expected).abs() < 0.003,
                    "A calibration: {got} vs {expected}"
                );
            }
        }
    }

    #[test]
    fn xtrans_rejects_invalid_levels_crop_topology_and_cancellation() {
        let (_fixture, raw) = fixture(&CFA::new(XTRANS), false, false);
        assert!(develop_xtrans(&raw, &|| true)
            .err()
            .expect("cancelled before development")
            .is::<crate::RawExportCancelled>());
        let mut invalid = raw.clone();
        invalid.whitelevel.0 = vec![1];
        assert!(develop_xtrans(&invalid, &|| false).is_err());
        let mut invalid = raw.clone();
        invalid.crop_area = Some(Rect::new(
            Point::zero(),
            rawler::imgop::Dim2::new(raw.width + 1, raw.height),
        ));
        assert!(develop_xtrans(&invalid, &|| false).is_err());
        let mut invalid = raw;
        let RawPhotometricInterpretation::Cfa(ref mut config) = invalid.photometric else {
            unreachable!()
        };
        config.cfa = CFA::new("RRRRRRGGGGGGBBBBBBRRRRRRGGGGGGBBBBBB");
        assert!(develop_xtrans(&invalid, &|| false).is_err());
    }
}
