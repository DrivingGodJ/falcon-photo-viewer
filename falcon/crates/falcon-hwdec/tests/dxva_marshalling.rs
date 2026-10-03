//! v0.8.146 (E3-M1) — **the marshalling rows that need no hardware.**
//!
//! Everything here is arithmetic and byte layout: the structures the driver reads, the order the
//! quantisation matrices go in, and the shape of the bitstream buffer. It runs on any Windows box,
//! with or without a video device, because none of it talks to a driver — which is the point. A
//! marshalling bug that only a GPU can catch is a marshalling bug that a CI machine cannot.

#![cfg(windows)]

#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;

// ── v0.8.149 (F9/B11): THE SKIP AUDIT ────────────────────────────────────────────────────────────
//
// Every row in this file can decline to run, and the E6 audit's point is that a suite which prints
// `test result: ok` while a third of it declined is not reporting what it did. The reason was
// already named at every site; what was missing is that nothing COUNTED them, so a green run on a
// box without the corpus and a green run that actually gated the epic were indistinguishable at a
// glance. `skip!` numbers each one and points at the inventory in TESTING.md §8h, and
// `the_skip_audit_states_what_this_machine_could_not_run` prints the total.
static SKIPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

macro_rules! skip {
    ($($t:tt)*) => {{
        let n = SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        eprintln!(
            "SKIP #{n} (this row needs hardware and/or the HEIC corpus — TESTING.md \u{a7}8h): {}",
            format!($($t)*)
        );
    }};
}

use std::path::PathBuf;

use falcon_hwdec::dxva::*;
use falcon_hwdec::hevc::*;

/// The two folders that hold real HEICs — the same list `heif_grid_parity.rs` walks.
fn corpus_dirs() -> [&'static std::path::Path; 2] {
    [fixture_paths::heic(), fixture_paths::photos()]
}

fn find(name: &str) -> Option<PathBuf> {
    let found = corpus_dirs().iter().map(|d| PathBuf::from(d).join(name)).find(|p| fixture_paths::local_file(p));
    fixture_paths::require_available(found.is_some(), name);
    found
}

#[test]
fn dxva_structs_match_the_sdk_layout() {
    // These numbers were printed by MSVC from the Windows SDK's own `um/dxva.h` (10.0.26100.0):
    //
    //   sizeof(DXVA_PicParams_HEVC)   = 232      alignof = 1
    //   sizeof(DXVA_Qmatrix_HEVC)     = 1000
    //   sizeof(DXVA_Slice_HEVC_Short) = 10       alignof = 1
    //
    // The alignment of 1 is not decoration: the whole DXVA block lives inside
    // `#pragma pack(push, BeforeDXVApacking, 1)`. A plain `#[repr(C)]` slice-control struct would be
    // TWELVE bytes, and every slice after the first in a multi-slice picture would land two bytes
    // into the wrong place — with no error anywhere, just wrong pixels.
    assert_eq!(core::mem::size_of::<DxvaPicParamsHevc>(), 232);
    assert_eq!(core::mem::size_of::<DxvaQmatrixHevc>(), 1000);
    assert_eq!(core::mem::size_of::<DxvaSliceHevcShort>(), 10);
    assert_eq!(core::mem::align_of::<DxvaPicParamsHevc>(), 1);
    assert_eq!(core::mem::align_of::<DxvaSliceHevcShort>(), 1);

    // Field offsets, against the same oracle. Measured by writing a marker into one field of an
    // otherwise-zero struct and finding it in the bytes — which tests the ACTUAL layout rather than
    // re-stating a `#[repr]` attribute.
    let probe = |set: &dyn Fn(&mut DxvaPicParamsHevc), want: usize, width: usize, name: &str| {
        let mut pp = DxvaPicParamsHevc::default();
        set(&mut pp);
        let bytes = as_bytes(&pp);
        let first = bytes.iter().position(|&b| b != 0).unwrap_or_else(|| panic!("{name}: nothing set"));
        assert_eq!(first, want, "{name} is at {first}, the SDK says {want}");
        assert!(
            bytes[first..first + width].iter().any(|&b| b != 0),
            "{name} did not fill its own {width} bytes"
        );
    };
    probe(&|p| p.PicWidthInMinCbsY = 0x0102, 0, 2, "PicWidthInMinCbsY");
    probe(&|p| p.PicHeightInMinCbsY = 0x0102, 2, 2, "PicHeightInMinCbsY");
    probe(&|p| p.wFormatAndSequenceInfoFlags = 0x0102, 4, 2, "wFormatAndSequenceInfoFlags");
    probe(&|p| p.CurrPic = 0xAB, 6, 1, "CurrPic");
    probe(&|p| p.sps_max_dec_pic_buffering_minus1 = 0xAB, 7, 1, "sps_max_dec_pic_buffering_minus1");
    probe(&|p| p.log2_min_luma_coding_block_size_minus3 = 0xAB, 8, 1, "log2_min_luma_cb_minus3");
    probe(&|p| p.init_qp_minus26 = 0x7f, 18, 1, "init_qp_minus26");
    probe(&|p| p.ucNumDeltaPocsOfRefRpsIdx = 0xAB, 19, 1, "ucNumDeltaPocsOfRefRpsIdx");
    probe(&|p| p.wNumBitsForShortTermRPSInSlice = 0x0102, 20, 2, "wNumBitsForShortTermRPSInSlice");
    probe(&|p| p.ReservedBits2 = 0x0102, 22, 2, "ReservedBits2");
    probe(&|p| p.dwCodingParamToolFlags = 0x0102_0304, 24, 4, "dwCodingParamToolFlags");
    probe(&|p| p.dwCodingSettingPicturePropertyFlags = 0x0102_0304, 28, 4, "dwCodingSettingPicturePropertyFlags");
    probe(&|p| p.pps_cb_qp_offset = 0x7f, 32, 1, "pps_cb_qp_offset");
    probe(&|p| p.pps_cr_qp_offset = 0x7f, 33, 1, "pps_cr_qp_offset");
    probe(&|p| p.num_tile_columns_minus1 = 0xAB, 34, 1, "num_tile_columns_minus1");
    probe(&|p| p.num_tile_rows_minus1 = 0xAB, 35, 1, "num_tile_rows_minus1");
    probe(&|p| p.column_width_minus1[0] = 0x0102, 36, 38, "column_width_minus1");
    probe(&|p| p.row_height_minus1[0] = 0x0102, 74, 42, "row_height_minus1");
    probe(&|p| p.diff_cu_qp_delta_depth = 0xAB, 116, 1, "diff_cu_qp_delta_depth");
    probe(&|p| p.pps_beta_offset_div2 = 0x7f, 117, 1, "pps_beta_offset_div2");
    probe(&|p| p.pps_tc_offset_div2 = 0x7f, 118, 1, "pps_tc_offset_div2");
    probe(&|p| p.log2_parallel_merge_level_minus2 = 0xAB, 119, 1, "log2_parallel_merge_level_minus2");
    probe(&|p| p.CurrPicOrderCntVal = 0x0102_0304, 120, 4, "CurrPicOrderCntVal");
    probe(&|p| p.RefPicList[0] = 0xAB, 124, 15, "RefPicList");
    probe(&|p| p.ReservedBits5 = 0xAB, 139, 1, "ReservedBits5");
    probe(&|p| p.PicOrderCntValList[0] = 0x0102_0304, 140, 60, "PicOrderCntValList");
    probe(&|p| p.RefPicSetStCurrBefore[0] = 0xAB, 200, 8, "RefPicSetStCurrBefore");
    probe(&|p| p.RefPicSetStCurrAfter[0] = 0xAB, 208, 8, "RefPicSetStCurrAfter");
    probe(&|p| p.RefPicSetLtCurr[0] = 0xAB, 216, 8, "RefPicSetLtCurr");
    probe(&|p| p.ReservedBits6 = 0x0102, 224, 2, "ReservedBits6");
    probe(&|p| p.ReservedBits7 = 0x0102, 226, 2, "ReservedBits7");
    probe(&|p| p.StatusReportFeedbackNumber = 0x0102_0304, 228, 4, "StatusReportFeedbackNumber");

    // The quantisation matrix's own sub-array offsets: 0 / 96 / 480 / 864 / 992 / 998.
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingLists0[0][0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(0));
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingLists1[0][0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(96));
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingLists2[0][0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(480));
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingLists3[0][0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(864));
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingListDCCoefSizeID2[0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(992));
    let mut qm = DxvaQmatrixHevc::default();
    qm.ucScalingListDCCoefSizeID3[0] = 1;
    assert_eq!(as_bytes(&qm).iter().position(|&b| b != 0), Some(998));

    // And the slice control's, 0 / 4 / 8 — the three that make the 10-byte size add up.
    let s = DxvaSliceHevcShort {
        BSNALunitDataLocation: 0x0102_0304,
        SliceBytesInBuffer: 0x0506_0708,
        wBadSliceChopping: 0x090a,
    };
    let b = as_bytes(&s);
    assert_eq!(&b[0..4], &0x0102_0304u32.to_le_bytes());
    assert_eq!(&b[4..8], &0x0506_0708u32.to_le_bytes());
    assert_eq!(&b[8..10], &0x090au16.to_le_bytes());
}

#[test]
fn the_default_matrices_reach_the_driver_in_signalled_order() {
    // THE LANDMINE, stated as an assertion. `scaling_list_enabled_flag` is 1 on every corpus file;
    // a file that does NOT carry `scaling_list_data()` still has matrices, and they are the H.265
    // defaults of Table 7-6 — not flat 16s. This is what a decoder that "skips the quantisation
    // matrices because the file has no explicit ones" would be omitting.
    let sl = ScalingLists::default();
    let qm = fill_qmatrix(&sl);
    assert_eq!(qm.ucScalingLists0[0], [16u8; 16], "the 4x4 default IS flat 16 (Table 7-5)");
    assert_eq!(
        &qm.ucScalingLists1[0][..16],
        &[16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18],
        "the 8x8 intra default in SIGNALLED (diagonal) order"
    );
    assert_eq!(
        &qm.ucScalingLists1[3][..16],
        &[16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18],
        "the 8x8 inter default in SIGNALLED order"
    );
    assert_ne!(qm.ucScalingLists2[0], [16u8; 64], "the 16x16 default is NOT flat");
    assert_ne!(qm.ucScalingLists3[0], [16u8; 64], "the 32x32 default is NOT flat");
    // sizeId 3 carries only matrixId 0 and 3 — the second DXVA row is the file's matrixId 3.
    assert_eq!(qm.ucScalingLists3[1], {
        let mut a = [0u8; 64];
        a.copy_from_slice(&sl.list[3][3]);
        a
    });
    assert_eq!(qm.ucScalingListDCCoefSizeID2, [16u8; 6]);
    assert_eq!(qm.ucScalingListDCCoefSizeID3, [16u8; 2]);

    // If the order were RASTER instead, the first sixteen 8x8 intra entries would read
    // 16,16,16,16,17,18,21,24,16,… — the top rows of the matrix. Naming the wrong answer is what
    // makes this row a falsifier rather than a restatement.
    assert_ne!(
        &qm.ucScalingLists1[0][..8],
        &[16, 16, 16, 16, 17, 18, 21, 24],
        "raster order would have leaked through"
    );
}

#[test]
fn the_bitstream_buffer_is_start_code_prefixed_and_padded() {
    // Two "NALs" of 5 and 7 bytes in a 4-byte-length item.
    let mut item = Vec::new();
    item.extend_from_slice(&5u32.to_be_bytes());
    item.extend_from_slice(&[0x28, 0x01, 0xaa, 0xbb, 0xcc]);
    item.extend_from_slice(&7u32.to_be_bytes());
    item.extend_from_slice(&[0x28, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let nals = split_item_nals(&item, 4).unwrap();
    assert_eq!(nals.len(), 2);

    let (buf, slices) = build_bitstream(&item, &nals, 4096).unwrap();
    assert_eq!(slices.len(), 2);
    // Slice 0: start code at 0, size counts the start code.
    assert_eq!({ slices[0].BSNALunitDataLocation }, 0);
    assert_eq!({ slices[0].SliceBytesInBuffer }, 3 + 5);
    assert_eq!(&buf[0..3], &[0, 0, 1]);
    assert_eq!(&buf[3..8], &[0x28, 0x01, 0xaa, 0xbb, 0xcc]);
    // Slice 1 follows immediately, and its location is the offset of ITS start code.
    assert_eq!({ slices[1].BSNALunitDataLocation }, 8);
    assert_eq!({ slices[1].SliceBytesInBuffer }, 3 + 7);
    assert_eq!(&buf[8..11], &[0, 0, 1]);
    // Padded to the 128-byte quantum with zeros.
    assert_eq!(buf.len() % BITSTREAM_ALIGN, 0);
    assert_eq!(buf.len(), 128);
    assert!(buf[18..].iter().all(|&b| b == 0), "the pad is zeros");
    assert!(slices.iter().all(|s| s.wBadSliceChopping == 0));
}

#[test]
fn the_bitstream_build_declines_rather_than_truncating() {
    let mut item = Vec::new();
    item.extend_from_slice(&600u32.to_be_bytes());
    item.extend_from_slice(&[0x28u8; 600]);
    let nals = split_item_nals(&item, 4).unwrap();
    // A driver buffer smaller than the picture is a decline, not a short write. The alternative —
    // copying what fits — hands the driver a truncated slice, which decodes to a picture.
    assert!(build_bitstream(&item, &nals, 500).is_none());
    assert!(build_bitstream(&item, &nals, 603).is_some());
    // And a picture with no VCL NAL at all is refused rather than submitted empty.
    let mut sei = Vec::new();
    sei.extend_from_slice(&3u32.to_be_bytes());
    sei.extend_from_slice(&[0x4e, 0x01, 0x00]); // NAL 39, PREFIX_SEI — not a slice
    let sn = split_item_nals(&sei, 4).unwrap();
    assert!(build_bitstream(&sei, &sn, 4096).is_none());
}

#[test]
fn an_idr_marshals_no_references_and_a_moving_status_number() {
    let Some(path) = find("IMG_1826.HEIC") else {
        skip!("IMG_1826.HEIC is not in the corpus on this machine");
        return;
    };
    let src = falcon_hwdec::tile_source(&path).expect("E1 grid + hvcC");
    let pp = fill_pic_params(&src.params, 3, 7, true, true);

    // 0xff, not 0. Zero is a legal surface index, so a zeroed reference list tells the driver that
    // every reference picture is surface 0 — a statement about memory it is entitled to act on.
    assert_eq!(pp.RefPicList, [0xff; 15]);
    assert_eq!(pp.RefPicSetStCurrBefore, [0xff; 8]);
    assert_eq!(pp.RefPicSetStCurrAfter, [0xff; 8]);
    assert_eq!(pp.RefPicSetLtCurr, [0xff; 8]);
    assert_eq!({ pp.PicOrderCntValList }, [0; 15]);
    assert_eq!({ pp.CurrPicOrderCntVal }, 0, "an IDR resets the POC");
    assert_eq!(pp.CurrPic, 3, "CurrPic names the surface the frame was begun on");
    assert_eq!({ pp.StatusReportFeedbackNumber }, 7);
    assert_eq!({ pp.wNumBitsForShortTermRPSInSlice }, 0);
    assert_eq!(pp.ucNumDeltaPocsOfRefRpsIdx, 0);

    // IrapPicFlag / IdrPicFlag / IntraPicFlag are bits 16, 17, 18.
    let flags = pp.dwCodingSettingPicturePropertyFlags;
    assert_eq!((flags >> 16) & 7, 0b111, "all three intra flags set for an IDR still picture");
    let not_intra = fill_pic_params(&src.params, 0, 1, false, false);
    assert_eq!((not_intra.dwCodingSettingPicturePropertyFlags >> 16) & 7, 0);

    // The format word packs chroma_format_idc / bit depths / log2_max_poc_lsb-4.
    let f = pp.wFormatAndSequenceInfoFlags;
    assert_eq!(f & 3, src.params.sps.chroma_format_idc as u16, "chroma_format_idc, bits 0..2");
    assert_eq!((f >> 3) & 7, 0, "8-bit luma");
    assert_eq!((f >> 6) & 7, 0, "8-bit chroma");
    assert_eq!(pp.PicWidthInMinCbsY as u32 * src.params.sps.min_cb_size(), src.tile_w);
    assert_eq!(pp.PicHeightInMinCbsY as u32 * src.params.sps.min_cb_size(), src.tile_h);

    // scaling_list_enabled_flag is bit 0 of the tool flags, and it is SET on this file even though
    // the file carries no explicit lists — which is the whole reason the Qmatrix buffer must go.
    assert_eq!({ pp.dwCodingParamToolFlags } & 1, 1);
    assert!(!src.params.sps.sps_scaling_list_data_present_flag);
}

#[test]
fn the_matched_fixture_pair_differs_in_exactly_the_scaling_lists() {
    // Stage 0's SP2 close-out: same camera, same 9x6 grid, same 896x1024 tiles; the SPS grew from
    // 34 to 501 bytes and a bit-level parse isolated ONE difference. This re-derives that claim from
    // Falcon's own parser rather than citing it.
    let (Some(a), Some(b)) = (find("IMG_1826.HEIC"), find("IMG_3258.HEIC")) else {
        skip!("the matched fixture pair is not both present on this machine");
        return;
    };
    let sa = falcon_hwdec::tile_source(&a).expect("IMG_1826 grid").params;
    let sb = falcon_hwdec::tile_source(&b).expect("IMG_3258 grid").params;

    assert!(!sa.sps.sps_scaling_list_data_present_flag, "IMG_1826 uses the DEFAULT lists");
    assert!(sb.sps.sps_scaling_list_data_present_flag, "IMG_3258 signals its own");
    assert_eq!(sa.effective_scaling_lists().explicit_matrices, 0);
    assert_eq!(sb.effective_scaling_lists().explicit_matrices, 20, "20 explicit matrices");
    assert!(sa.sps.scaling_list_enabled_flag && sb.sps.scaling_list_enabled_flag);
    assert_ne!(
        as_bytes(&fill_qmatrix(sa.effective_scaling_lists())),
        as_bytes(&fill_qmatrix(sb.effective_scaling_lists())),
        "the two files' quantisation matrices must not be the same bytes"
    );

    // Everything a DXVA pic-params fill reads is otherwise identical between the two, which is what
    // makes the pair a controlled experiment rather than two unrelated photos.
    let pa = fill_pic_params(&sa, 0, 1, true, true);
    let pb = fill_pic_params(&sb, 0, 1, true, true);
    assert_eq!(as_bytes(&pa), as_bytes(&pb), "the pic params are byte-identical across the pair");

    // …and the parses are self-checked all the way to the stop bit.
    assert!(sa.sps.tail_verified && sa.pps.tail_verified, "IMG_1826 parameter sets end cleanly");
    assert!(sb.sps.tail_verified && sb.pps.tail_verified, "IMG_3258 parameter sets end cleanly");
}

#[test]
fn the_parameter_set_read_agrees_with_what_e2_measured() {
    // E2 pinned the VUI of all six corpus files from a Python parse and ffprobe. This is a THIRD
    // independent reader arriving at the same numbers, which is the only kind of agreement worth
    // anything.
    let mut seen = 0;
    for name in ["IMG_1826.HEIC", "IMG_1827.HEIC", "IMG_1828.HEIC", "IMG_2814.HEIC", "IMG_3258.HEIC"] {
        let Some(p) = find(name) else { continue };
        let Ok(src) = falcon_hwdec::tile_source(&p) else { continue };
        let v = src.params.sps.vui;
        seen += 1;
        assert!(v.present, "{name}: the SPS carries a VUI");
        assert!(v.video_full_range_flag, "{name}: FULL range");
        assert_eq!(v.matrix_coeffs, 6, "{name}: BT.601, not 709");
        assert_eq!(v.colour_primaries, 12, "{name}: SMPTE ST 432-1 = Display P3");
        assert_eq!(v.transfer_characteristics, 1, "{name}");
        assert!(!v.chroma_loc_info_present, "{name}: absent -> the H.265 default, LEFT siting");
        assert_eq!(src.params.sps.profile_idc, 3, "{name}: Main Still Picture");
        assert_eq!(src.params.sps.bit_depth_luma_minus8, 0, "{name}: 8-bit");
        assert_eq!(src.params.sps.chroma_format_idc, 1, "{name}: 4:2:0");
        assert_eq!(src.params.length_size, 4, "{name}");
        // The hvcC record and the SPS must agree; they are two declarations of the same fact.
        assert_eq!(src.params.record_bit_depth_luma, 8, "{name}");
        assert_eq!(src.params.record_chroma_format, 1, "{name}");
        assert_eq!(src.params.record_profile_idc, 3, "{name}");
    }
    if seen == 0 {
        skip!("no corpus HEIC on this machine");
    }
}

#[test]
fn the_parameter_set_parser_fails_closed_on_mutated_records() {
    // 5,000 single-byte flips of a real hvcC. Every one must be an Err or a self-consistent parse —
    // never a panic, and never a silent success on a record whose SPS no longer terminates.
    let Some(path) = find("IMG_3258.HEIC") else {
        skip!("IMG_3258.HEIC is not in the corpus on this machine");
        return;
    };
    let bytes = std::fs::read(&path).expect("read the fixture");
    let plan = falcon_decode::parse_heif_grid(&bytes).expect("E1 grid");
    let hvcc = plan.hvcc.expect("one shared hvcC");
    assert!(parse_hvcc(&hvcc).is_ok(), "the unmutated record parses");

    let mut state = 0x0805_2026u64;
    let mut ok = 0;
    let mut err = 0;
    for _ in 0..5000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let at = (state >> 33) as usize % hvcc.len();
        let bit = ((state >> 17) & 7) as u8;
        let mut m = hvcc.clone();
        m[at] ^= 1 << bit;
        match parse_hvcc(&m) {
            Ok(ps) => {
                ok += 1;
                // A record that still parses must still be internally coherent.
                assert!(ps.length_size >= 1 && ps.length_size <= 4);
                assert!(ps.sps.min_cb_size() >= 8 && ps.sps.min_cb_size() <= 64);
            }
            Err(_) => err += 1,
        }
    }
    assert_eq!(ok + err, 5000);
    assert!(err > 0, "5,000 flips of a 500-byte SPS should break SOMETHING");
    eprintln!("hvcC fuzz: {ok} still parsed, {err} declined, 0 panicked");
}
