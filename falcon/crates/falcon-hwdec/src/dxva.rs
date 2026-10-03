//! v0.8.146 (E3-M1) — **the DXVA HEVC marshalling**: the three structures a driver decodes from,
//! built from the parameter sets [`crate::hevc`] read out of the file.
//!
//! # The layout is the SDK's, not a guess
//!
//! These are hand-written Rust mirrors of `DXVA_PicParams_HEVC`, `DXVA_Qmatrix_HEVC` and
//! `DXVA_Slice_HEVC_Short` from the Windows SDK's `um/dxva.h` (10.0.26100.0). `windows-rs` does not
//! generate them — `d3d11.h`'s decoder API takes `void*` payloads and the *contents* are the DXVA
//! HEVC specification's business — so the layout is ours to get right, and getting it wrong is
//! silent: the driver reads whatever bytes sit at the offset it expects.
//!
//! Two things in that header are traps, and both were read rather than assumed:
//!
//! * The whole DXVA block sits inside `#pragma pack(push, BeforeDXVApacking, 1)`. Everything here
//!   is therefore `#[repr(C, packed(1))]`. `DXVA_Slice_HEVC_Short` is **10 bytes, not 12** — a plain
//!   `#[repr(C)]` `{u32, u32, u16}` would be 12, and every slice past the first in a multi-slice
//!   picture would land two bytes off.
//! * `ReservedBits5` is a `UCHAR`, not the `USHORT` its neighbours suggest, which is what keeps
//!   `PicOrderCntValList` at offset 140.
//!
//! `dxva_structs_match_the_sdk_layout` pins the sizes and every interesting offset against numbers
//! printed by MSVC from the SDK header itself (232 / 1000 / 10). See the test for the whole table.
//!
//! # The quantisation-matrix landmine
//!
//! Stage 0 identified, and the matched fixture pair exists to prove, that `DXVA_Qmatrix_HEVC` must
//! be populated from the SPS/PPS scaling lists. Two independent ways to get this wrong:
//!
//! 1. **Skipping the buffer.** `scaling_list_enabled_flag` is 1 on every corpus file. When the SPS
//!    does not also carry `scaling_list_data()`, the lists are the H.265 DEFAULTS — the graded
//!    matrices of Table 7-6, **not flat 16s** — so "no explicit lists" is not "no matrices".
//! 2. **The wrong order.** `ScalingList[sizeId][matrixId][i]` is indexed by the up-right diagonal
//!    scan position, and that is the order DXVA wants. ffmpeg keeps its own copy in raster order
//!    and converts BACK through the diagonal tables in `ff_dxva2_hevc_fill_scaling_lists`; Falcon
//!    stores signalled order throughout and copies straight across. Both orders decode 24 matrices
//!    without complaint and only one of them decodes the right picture.
//!
//! [`QmatrixPolicy`] exists so both mistakes can be MADE ON PURPOSE and measured — the falsifier
//! that proves the landmine is armed. Nothing but a test ever passes anything but
//! [`QmatrixPolicy::FromParameterSets`].

use crate::hevc::{ParameterSets, ScalingLists};

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The structures
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `DXVA_PicParams_HEVC` (SDK `um/dxva.h`, 232 bytes, packed to 1).
///
/// The bit-field unions of the C original are carried as their `wFormatAndSequenceInfoFlags` /
/// `dwCodingParamToolFlags` / `dwCodingSettingPicturePropertyFlags` whole-word aliases, because
/// that is the half of the union whose layout is defined rather than compiler-dependent.
#[repr(C, packed(1))]
#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub struct DxvaPicParamsHevc {
    pub PicWidthInMinCbsY: u16,
    pub PicHeightInMinCbsY: u16,
    pub wFormatAndSequenceInfoFlags: u16,
    pub CurrPic: u8,
    pub sps_max_dec_pic_buffering_minus1: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_transform_block_size_minus2: u8,
    pub log2_diff_max_min_transform_block_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pics_sps: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub init_qp_minus26: i8,
    pub ucNumDeltaPocsOfRefRpsIdx: u8,
    pub wNumBitsForShortTermRPSInSlice: u16,
    pub ReservedBits2: u16,
    pub dwCodingParamToolFlags: u32,
    pub dwCodingSettingPicturePropertyFlags: u32,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u16; 19],
    pub row_height_minus1: [u16; 21],
    pub diff_cu_qp_delta_depth: u8,
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub CurrPicOrderCntVal: i32,
    pub RefPicList: [u8; 15],
    pub ReservedBits5: u8,
    pub PicOrderCntValList: [i32; 15],
    pub RefPicSetStCurrBefore: [u8; 8],
    pub RefPicSetStCurrAfter: [u8; 8],
    pub RefPicSetLtCurr: [u8; 8],
    pub ReservedBits6: u16,
    pub ReservedBits7: u16,
    pub StatusReportFeedbackNumber: u32,
}

impl Default for DxvaPicParamsHevc {
    fn default() -> Self {
        // All-zero is the C `memset(pp, 0, sizeof(*pp))` every reference implementation starts from.
        unsafe { core::mem::zeroed() }
    }
}

/// `DXVA_Qmatrix_HEVC` (SDK `um/dxva.h`, 1000 bytes, packed to 1).
#[repr(C, packed(1))]
#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub struct DxvaQmatrixHevc {
    pub ucScalingLists0: [[u8; 16]; 6],
    pub ucScalingLists1: [[u8; 64]; 6],
    pub ucScalingLists2: [[u8; 64]; 6],
    pub ucScalingLists3: [[u8; 64]; 2],
    pub ucScalingListDCCoefSizeID2: [u8; 6],
    pub ucScalingListDCCoefSizeID3: [u8; 2],
}

impl Default for DxvaQmatrixHevc {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

/// `DXVA_Slice_HEVC_Short` (SDK `um/dxva.h`, **10** bytes, packed to 1).
#[repr(C, packed(1))]
#[derive(Clone, Copy, Default)]
#[allow(non_snake_case)]
pub struct DxvaSliceHevcShort {
    /// Byte offset of the slice's START CODE within the bitstream buffer.
    pub BSNALunitDataLocation: u32,
    /// Start code INCLUDED — the size of what was written at that offset.
    pub SliceBytesInBuffer: u32,
    pub wBadSliceChopping: u16,
}

/// The private supertrait that SEALS [`DxvaPod`]: it cannot be named outside this crate, so no
/// downstream `unsafe impl` can add a fourth type to the set. v0.8.152 (R3-M2).
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::DxvaPicParamsHevc {}
    impl Sealed for super::DxvaQmatrixHevc {}
    impl Sealed for super::DxvaSliceHevcShort {}
}

/// The three DXVA payload structs, and nothing else. v0.8.152 (R3-M2).
///
/// # Why a trait rather than a comment
///
/// [`as_bytes`] used to be generic over any `T: Copy` with the invariant STATED in a `// SAFETY:`
/// line and UPHELD by nothing. `dxva` is a `pub mod` on a workspace crate, so any `T` with padding
/// bytes — which is every ordinary `repr(Rust)` or non-packed `repr(C)` struct — would have read
/// uninitialised memory (UB), and any `T` containing a reference would have written a live pointer
/// into a driver buffer. In-tree only the three `packed(1)` structs were ever passed, which made it
/// a soundness HAZARD rather than a live bug — and the single place in this partition where an
/// `unsafe` block's precondition crossed a public boundary on convention alone.
///
/// `as_bytes(&(1u8, 2u32))` — the finding's falsifier, which compiled and read three padding bytes
/// — is now a compile error, pinned by the `compile_fail` doctest on [`as_bytes`].
///
/// # Safety
///
/// An implementor must be `#[repr(C, packed(1))]` (or otherwise padding-free), must contain no
/// references, no pointers and no interior mutability, and every one of its `size_of` bytes must be
/// initialised data — because that is exactly what [`as_bytes`] hands the driver.
#[doc(hidden)]
pub unsafe trait DxvaPod: Copy + sealed::Sealed {}

// SAFETY: all three are `#[repr(C, packed(1))]` aggregates of integers and integer arrays — no
// padding, no references, no interior mutability — and every field is written before submission
// (`Default` is a `mem::zeroed`, so even an untouched one is initialised).
unsafe impl DxvaPod for DxvaPicParamsHevc {}
unsafe impl DxvaPod for DxvaQmatrixHevc {}
unsafe impl DxvaPod for DxvaSliceHevcShort {}

/// View any of the three as the bytes the driver will read. Safe because each is `packed(1)` with
/// no padding and no pointers — every byte of the struct is initialised data, which is what
/// [`DxvaPod`] now makes the compiler check instead of the reader.
///
/// The three that are allowed:
///
/// ```
/// # #[cfg(windows)] {
/// use falcon_hwdec::dxva::{as_bytes, DxvaSliceHevcShort};
/// assert_eq!(as_bytes(&DxvaSliceHevcShort::default()).len(), 10);
/// # }
/// ```
///
/// …and everything else, which is R3-M2's falsifier and no longer compiles.
///
/// v0.8.153 (skeptic A / Y8): that second doctest exists only on Windows, written as conditional
/// `doc` attributes because a doctest cannot be `cfg`'d from inside. The positive one can use the
/// `# #[cfg(windows)] {` trick — it still has to COMPILE off Windows, and an empty body does. A
/// `compile_fail` block cannot: `dxva` is `#[cfg(windows)]`, so off Windows the `use` line alone
/// fails and the test passes for a reason that has nothing to do with `DxvaPod`. It claimed the
/// seal held on platforms where the seal does not exist. Now it claims nothing there.
#[cfg_attr(windows, doc = "")]
#[cfg_attr(windows, doc = "```compile_fail")]
#[cfg_attr(windows, doc = "use falcon_hwdec::dxva::as_bytes;")]
#[cfg_attr(windows, doc = "// `(u8, u32)` has three padding bytes; reading them is UB.")]
#[cfg_attr(windows, doc = "let _ = as_bytes(&(1u8, 2u32));")]
#[cfg_attr(windows, doc = "```")]
pub fn as_bytes<T: DxvaPod>(v: &T) -> &[u8] {
    // SAFETY: `T: DxvaPod` is the sealed set of the three `packed(1)` POD structs above; they
    // contain no padding, no references and no interior mutability, so their whole footprint is
    // readable, initialised bytes.
    unsafe { core::slice::from_raw_parts((v as *const T).cast::<u8>(), core::mem::size_of::<T>()) }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The fill
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The lever's one definition. It lives in a private module so that v0.8.152's `falsifiers` gate
/// (ruling 5.1(b)) can be a re-export at two different visibilities rather than two copies of an
/// enum that would drift.
mod qpolicy {
    /// What to put in the quantisation-matrix buffer. Only [`Self::FromParameterSets`] is correct;
    /// the other two exist to be measured against it (see the module header).
    ///
    /// Without the `falsifiers` feature the two wrong policies are never CONSTRUCTED — the session
    /// still matches on them, but nothing can hand one in. That is the gate working, not dead
    /// code, so the shipping build is told to expect it.
    #[cfg_attr(not(feature = "falsifiers"), allow(dead_code))]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum QmatrixPolicy {
        /// The scaling lists the file declares — PPS's if present, else the SPS's, which are the
        /// H.265 defaults when the SPS did not signal any.
        #[default]
        FromParameterSets,
        /// FALSIFIER ONLY: send an all-zero `DXVA_Qmatrix_HEVC`. The buffer is present and the
        /// matrices are wrong.
        Zeroed,
        /// FALSIFIER ONLY: do not send the quantisation-matrix buffer at all, and let the driver
        /// decide what the matrices are.
        Omitted,
    }
}

/// v0.8.152 (5.1(b)): visible — and `#[doc(hidden)]`, so it never appears in the crate's docs —
/// only when the `falsifiers` feature is on, which is only ever when a test or example target is
/// being built. See the feature's stanza in `Cargo.toml` for why that is the whole mechanism.
#[cfg(feature = "falsifiers")]
#[doc(hidden)]
pub use qpolicy::QmatrixPolicy;
/// The shipping build: the enum still exists (the session's own signatures take it), it simply
/// cannot be NAMED from outside this crate, so `native/src/hwheic.rs` reaching for
/// `QmatrixPolicy::Zeroed` is a compile error rather than a review comment.
#[cfg(not(feature = "falsifiers"))]
pub(crate) use qpolicy::QmatrixPolicy;

/// Fill `DXVA_Qmatrix_HEVC` from `ScalingList[sizeId][matrixId][i]` in signalled order.
///
/// The index mapping is a straight copy in this direction — see the module header for why that is
/// the interesting fact rather than a triviality. sizeId 3 signals only matrixId 0 and 3, which is
/// why the last array is indexed `i * 3`.
pub fn fill_qmatrix(sl: &ScalingLists) -> DxvaQmatrixHevc {
    let mut qm = DxvaQmatrixHevc::default();
    for m in 0..6usize {
        qm.ucScalingLists0[m].copy_from_slice(&sl.list[0][m][..16]);
        qm.ucScalingLists1[m].copy_from_slice(&sl.list[1][m]);
        qm.ucScalingLists2[m].copy_from_slice(&sl.list[2][m]);
        qm.ucScalingListDCCoefSizeID2[m] = sl.dc[0][m];
        if m < 2 {
            qm.ucScalingLists3[m].copy_from_slice(&sl.list[3][m * 3]);
            qm.ucScalingListDCCoefSizeID3[m] = sl.dc[1][m * 3];
        }
    }
    qm
}

/// Fill `DXVA_PicParams_HEVC` for ONE intra still picture (an IDR with an empty DPB), decoding into
/// surface `surface_index`.
///
/// # The fields that took judgement
///
/// * **`CurrPic`** is `Index7Bits = surface_index`, `AssociatedFlag = 0`. The driver writes the
///   picture into the surface this names; the output view handed to `DecoderBeginFrame` must be the
///   same slice, and [`crate::session`] is what keeps those two in step.
/// * **`RefPicList` / `RefPicSetStCurrBefore` / `…After` / `…LtCurr`** are filled with `0xff`, not
///   zero. Zero is a valid surface index, so a zeroed reference list says "every reference is
///   surface 0" — for an IDR with nothing in the DPB that is a lie the driver is entitled to act
///   on. `0xff` is DXVA's "no entry".
/// * **`PicOrderCntValList`** stays 0 for the unused entries, which is what the reference does, and
///   `CurrPicOrderCntVal` is 0 because an IDR resets the POC.
/// * **`IrapPicFlag` / `IdrPicFlag` / `IntraPicFlag`** (bits 16/17/18) are all set. Every HEIC tile
///   in the corpus is an `IDR_N_LP` (NAL 20), so all three hold; `intra_picture` carries the caller's
///   answer rather than this function assuming it.
/// * **`wNumBitsForShortTermRPSInSlice` and `ucNumDeltaPocsOfRefRpsIdx`** are 0. They describe a
///   short-term reference-picture set written INSIDE the slice header, which an IDR never has.
/// * **`sps_max_dec_pic_buffering_minus1`** is the highest sub-layer's value, per the reference.
/// * **`pps_beta_offset_div2` / `pps_tc_offset_div2`** are the `_div2` syntax elements as parsed —
///   the reference divides because its own parse doubled them first.
/// * **`StatusReportFeedbackNumber`** must be non-zero and move; a session hands out 1, 2, 3, …
/// * **PCM sub-fields** are zeroed unless `pcm_enabled_flag`, because the SPS did not send them.
// Zero-then-assign, not a 60-field struct literal: it mirrors the reference's
// `memset(pp, 0, sizeof(*pp))` followed by named writes, and it means a field this function forgets
// is DEFINITELY zero rather than accidentally whatever the previous picture left there.
#[allow(clippy::field_reassign_with_default)]
pub fn fill_pic_params(
    ps: &ParameterSets,
    surface_index: u8,
    status_report: u32,
    intra_picture: bool,
    idr_picture: bool,
) -> DxvaPicParamsHevc {
    let sps = &ps.sps;
    let pps = &ps.pps;
    let mut pp = DxvaPicParamsHevc::default();

    pp.PicWidthInMinCbsY = sps.min_cb_width() as u16;
    pp.PicHeightInMinCbsY = sps.min_cb_height() as u16;
    pp.wFormatAndSequenceInfoFlags = (sps.chroma_format_idc as u16 & 0x3)
        | ((sps.separate_colour_plane_flag as u16) << 2)
        | ((sps.bit_depth_luma_minus8 as u16 & 0x7) << 3)
        | ((sps.bit_depth_chroma_minus8 as u16 & 0x7) << 6)
        | ((sps.log2_max_poc_lsb_minus4 as u16 & 0xf) << 9);
    // NoPicReorderingFlag (bit 13), NoBiPredFlag (bit 14) and ReservedBits1 (bit 15) stay 0: they
    // are hints, the reference leaves them 0, and a hint that is wrong is worse than one that is absent.

    pp.CurrPic = surface_index & 0x7f;
    pp.sps_max_dec_pic_buffering_minus1 = sps.max_dec_pic_buffering_minus1 as u8;
    pp.log2_min_luma_coding_block_size_minus3 = sps.log2_min_cb_size_minus3 as u8;
    pp.log2_diff_max_min_luma_coding_block_size = sps.log2_diff_max_min_cb_size as u8;
    pp.log2_min_transform_block_size_minus2 = sps.log2_min_tb_size_minus2 as u8;
    pp.log2_diff_max_min_transform_block_size = sps.log2_diff_max_min_tb_size as u8;
    pp.max_transform_hierarchy_depth_inter = sps.max_transform_hierarchy_depth_inter as u8;
    pp.max_transform_hierarchy_depth_intra = sps.max_transform_hierarchy_depth_intra as u8;
    pp.num_short_term_ref_pic_sets = sps.num_short_term_ref_pic_sets as u8;
    pp.num_long_term_ref_pics_sps = sps.num_long_term_ref_pics_sps as u8;
    pp.num_ref_idx_l0_default_active_minus1 = pps.num_ref_idx_l0_default_active_minus1 as u8;
    pp.num_ref_idx_l1_default_active_minus1 = pps.num_ref_idx_l1_default_active_minus1 as u8;
    pp.init_qp_minus26 = pps.init_qp_minus26 as i8;
    pp.ucNumDeltaPocsOfRefRpsIdx = 0;
    pp.wNumBitsForShortTermRPSInSlice = 0;

    let pcm = sps.pcm_enabled_flag;
    pp.dwCodingParamToolFlags = (sps.scaling_list_enabled_flag as u32)
        | ((sps.amp_enabled_flag as u32) << 1)
        | ((sps.sao_enabled_flag as u32) << 2)
        | ((pcm as u32) << 3)
        | (if pcm { sps.pcm_sample_bit_depth_luma_minus1 & 0xf } else { 0 } << 4)
        | (if pcm { sps.pcm_sample_bit_depth_chroma_minus1 & 0xf } else { 0 } << 8)
        | (if pcm { sps.log2_min_pcm_cb_size_minus3 & 0x3 } else { 0 } << 12)
        | (if pcm { sps.log2_diff_max_min_pcm_cb_size & 0x3 } else { 0 } << 14)
        | ((sps.pcm_loop_filter_disabled_flag as u32) << 16)
        | ((sps.long_term_ref_pics_present_flag as u32) << 17)
        | ((sps.temporal_mvp_enabled_flag as u32) << 18)
        | ((sps.strong_intra_smoothing_enabled_flag as u32) << 19)
        | ((pps.dependent_slice_segments_enabled_flag as u32) << 20)
        | ((pps.output_flag_present_flag as u32) << 21)
        | ((pps.num_extra_slice_header_bits & 0x7) << 22)
        | ((pps.sign_data_hiding_enabled_flag as u32) << 25)
        | ((pps.cabac_init_present_flag as u32) << 26);

    pp.dwCodingSettingPicturePropertyFlags = (pps.constrained_intra_pred_flag as u32)
        | ((pps.transform_skip_enabled_flag as u32) << 1)
        | ((pps.cu_qp_delta_enabled_flag as u32) << 2)
        | ((pps.pps_slice_chroma_qp_offsets_present_flag as u32) << 3)
        | ((pps.weighted_pred_flag as u32) << 4)
        | ((pps.weighted_bipred_flag as u32) << 5)
        | ((pps.transquant_bypass_enabled_flag as u32) << 6)
        | ((pps.tiles_enabled_flag as u32) << 7)
        | ((pps.entropy_coding_sync_enabled_flag as u32) << 8)
        | ((pps.uniform_spacing_flag as u32) << 9)
        | ((if pps.tiles_enabled_flag { pps.loop_filter_across_tiles_enabled_flag } else { false })
            as u32)
            << 10
        | ((pps.pps_loop_filter_across_slices_enabled_flag as u32) << 11)
        | ((pps.deblocking_filter_override_enabled_flag as u32) << 12)
        | ((pps.pps_deblocking_filter_disabled_flag as u32) << 13)
        | ((pps.lists_modification_present_flag as u32) << 14)
        | ((pps.slice_segment_header_extension_present_flag as u32) << 15)
        | ((intra_picture as u32) << 16)  // IrapPicFlag
        | ((idr_picture as u32) << 17)    // IdrPicFlag
        | ((intra_picture as u32) << 18); // IntraPicFlag

    pp.pps_cb_qp_offset = pps.pps_cb_qp_offset as i8;
    pp.pps_cr_qp_offset = pps.pps_cr_qp_offset as i8;
    if pps.tiles_enabled_flag {
        pp.num_tile_columns_minus1 = pps.num_tile_columns_minus1 as u8;
        pp.num_tile_rows_minus1 = pps.num_tile_rows_minus1 as u8;
        if !pps.uniform_spacing_flag {
            for (i, w) in pps.column_width_minus1.iter().take(19).enumerate() {
                pp.column_width_minus1[i] = *w as u16;
            }
            for (i, h) in pps.row_height_minus1.iter().take(21).enumerate() {
                pp.row_height_minus1[i] = *h as u16;
            }
        }
    }
    pp.diff_cu_qp_delta_depth = pps.diff_cu_qp_delta_depth as u8;
    pp.pps_beta_offset_div2 = pps.pps_beta_offset_div2 as i8;
    pp.pps_tc_offset_div2 = pps.pps_tc_offset_div2 as i8;
    pp.log2_parallel_merge_level_minus2 = pps.log2_parallel_merge_level_minus2 as u8;
    pp.CurrPicOrderCntVal = 0;

    pp.RefPicList = [0xff; 15];
    pp.RefPicSetStCurrBefore = [0xff; 8];
    pp.RefPicSetStCurrAfter = [0xff; 8];
    pp.RefPicSetLtCurr = [0xff; 8];
    pp.StatusReportFeedbackNumber = status_report;
    pp
}

/// The DXVA bitstream buffer's alignment quantum. The reference pads the final slice out to a
/// multiple of this with zeros; drivers are documented to expect it and at least one is known to
/// read past a short buffer.
pub const BITSTREAM_ALIGN: usize = 128;

/// Assemble the bitstream buffer and the matching slice-control array for one picture.
///
/// Each VCL NAL is written as a 3-byte Annex-B start code followed by the NAL's RAW bytes —
/// emulation-prevention bytes and all, because the driver's entropy decoder expects the escaped
/// form. `BSNALunitDataLocation` points at the start code and `SliceBytesInBuffer` counts it.
///
/// Returns `None` when the assembled picture would not fit in `capacity` (the driver-chosen
/// bitstream buffer size) — a decline, never a truncation.
pub fn build_bitstream(
    item: &[u8],
    nals: &[crate::hevc::ItemNal],
    capacity: usize,
) -> Option<(Vec<u8>, Vec<DxvaSliceHevcShort>)> {
    const START_CODE: [u8; 3] = [0, 0, 1];
    let mut buf: Vec<u8> = Vec::new();
    let mut slices = Vec::new();
    for n in nals.iter().filter(|n| n.is_vcl()) {
        let position = buf.len();
        let size = START_CODE.len() + n.len;
        if position + size > capacity {
            return None;
        }
        buf.extend_from_slice(&START_CODE);
        buf.extend_from_slice(item.get(n.start..n.start + n.len)?);
        slices.push(DxvaSliceHevcShort {
            BSNALunitDataLocation: position as u32,
            SliceBytesInBuffer: size as u32,
            wBadSliceChopping: 0,
        });
    }
    if slices.is_empty() {
        return None;
    }
    let pad = (BITSTREAM_ALIGN - (buf.len() % BITSTREAM_ALIGN)) % BITSTREAM_ALIGN;
    let pad = pad.min(capacity - buf.len());
    buf.resize(buf.len() + pad, 0);
    Some((buf, slices))
}
