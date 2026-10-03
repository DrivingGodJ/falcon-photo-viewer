//! v0.8.146 (E3-M1) — **the HEVC parameter sets, parsed from the file's own bytes.**
//!
//! DXVA does not take a bitstream and figure it out; it takes a `DXVA_PicParams_HEVC` in which
//! roughly sixty SPS/PPS fields have already been decoded for the driver, plus a
//! `DXVA_Qmatrix_HEVC` carrying the quantisation matrices. Every one of those fields is in the
//! `hvcC` record E1 already hands us. Nothing here is guessed, defaulted, or read from a name —
//! this module reads the bits (L42).
//!
//! # Fail closed, and REFUSE rather than approximate
//!
//! Every entry point returns `Result` and every read is bounds-checked, so a truncated or
//! adversarial parameter set is an `Err`, never a panic and never a silent partial parse. Where the
//! syntax has a branch this stage does not model — `inter_ref_pic_set_prediction_flag`, the SPS/PPS
//! extension flags, `separate_colour_plane_flag` — the answer is [`HwDecError::Unsupported`] with
//! the branch NAMED, not a best effort. A wrong parameter here does not fail loudly; it renders
//! wrong pixels, silently, which is precisely the failure mode this whole round exists to disarm.
//!
//! # The parse is self-checked
//!
//! [`parse_sps`] and [`parse_pps`] read all the way to `rbsp_trailing_bits()` — including the VUI
//! and its `hrd_parameters()` — and assert that the stop bit lands exactly where the syntax says it
//! must. A parser that mis-reads one `ue(v)` in the middle almost always desynchronises the tail,
//! so this one check catches the class of error that would otherwise be invisible until the pixels
//! came back subtly wrong. `sps_tail_verified` records the verdict rather than hiding it.

use crate::HwDecError;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The bit reader
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A checked MSB-first bit reader over an RBSP. Every read returns `Option`, so "the parameter set
/// ended early" is expressed by `?` and never by a panic.
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Bit position from the start of `bytes`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        BitReader { bytes, pos: 0 }
    }

    /// Bits consumed so far.
    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    /// Total bits available.
    pub fn bit_len(&self) -> usize {
        self.bytes.len() * 8
    }

    pub fn u(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            let byte = *self.bytes.get(self.pos >> 3)?;
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }

    pub fn flag(&mut self) -> Option<bool> {
        Some(self.u(1)? == 1)
    }

    /// `ue(v)` — unsigned Exp-Golomb. Refuses a prefix longer than 32 zeros, which is the only way
    /// a crafted parameter set could make this loop forever.
    pub fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 32 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        let rest = self.u(zeros)?;
        // (1 << zeros) - 1 + rest, in u64 so a 32-zero prefix cannot wrap before the check.
        let v = ((1u64 << zeros) - 1) + rest as u64;
        u32::try_from(v).ok()
    }

    /// `se(v)` — signed Exp-Golomb.
    pub fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) })
    }

    /// `rbsp_trailing_bits()` — exactly one `1` bit, then zeros to the byte boundary, then nothing.
    /// The whole-parse self-check described in the module header.
    fn trailing_bits_ok(&mut self) -> bool {
        if self.u(1) != Some(1) {
            return false;
        }
        while !self.pos.is_multiple_of(8) {
            if self.u(1) != Some(0) {
                return false;
            }
        }
        self.pos == self.bit_len()
    }
}

/// Strip emulation-prevention bytes: `00 00 03` → `00 00`. Operates on the NAL payload AFTER its
/// two-byte header, which is what the RBSP syntax is defined over.
pub fn unescape_rbsp(nal_payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal_payload.len());
    let mut zeros = 0usize;
    for &b in nal_payload {
        if zeros >= 2 && b == 0x03 {
            zeros = 0;
            continue;
        }
        out.push(b);
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Scaling lists — H.265 7.3.4 / Tables 7-5 and 7-6
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The 8×8 default INTRA scaling matrix in RASTER order (H.265 Table 7-6, laid out as the matrix it
/// actually is rather than as a scan). [`diag_scan_order`] converts it to the signalled order.
#[rustfmt::skip]
const DEFAULT_INTRA_RASTER: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 21, 24,
    16, 16, 16, 16, 17, 19, 22, 25,
    16, 16, 17, 18, 20, 22, 25, 29,
    16, 16, 18, 21, 24, 27, 31, 36,
    17, 17, 20, 24, 30, 35, 41, 47,
    18, 19, 22, 27, 35, 44, 54, 65,
    21, 22, 25, 31, 41, 54, 70, 88,
    24, 25, 29, 36, 47, 65, 88, 115,
];

/// The 8×8 default INTER scaling matrix in RASTER order (H.265 Table 7-6).
#[rustfmt::skip]
const DEFAULT_INTER_RASTER: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 20, 24,
    16, 16, 16, 17, 18, 20, 24, 25,
    16, 16, 17, 18, 20, 24, 25, 28,
    16, 17, 18, 20, 24, 25, 28, 33,
    17, 18, 20, 24, 25, 28, 33, 41,
    18, 20, 24, 25, 28, 33, 41, 54,
    20, 24, 25, 28, 33, 41, 54, 71,
    24, 25, 28, 33, 41, 54, 71, 91,
];

/// H.265 6.5.3 — the up-right diagonal scan order array initialisation process, run rather than
/// transcribed. Returns `blk_size²` `(x, y)` pairs; the raster index of entry `i` is `y·blk + x`.
///
/// Deriving this instead of pasting a table is what lets [`default_list`] state the default matrices
/// in the human-readable raster form above while [`ScalingLists`] stores them in the order the
/// bitstream signals them — and the order DXVA wants.
pub fn diag_scan_order(blk_size: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(blk_size * blk_size);
    let (mut x, mut y) = (0isize, 0isize);
    loop {
        while y >= 0 {
            if (x as usize) < blk_size && (y as usize) < blk_size {
                out.push((x as usize, y as usize));
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
        if out.len() >= blk_size * blk_size {
            break;
        }
    }
    out
}

/// The default `ScalingList[sizeId][matrixId][i]` in SIGNALLED (up-right diagonal) order.
fn default_list(size_id: usize, matrix_id: usize) -> [u8; 64] {
    let mut out = [16u8; 64];
    if size_id == 0 {
        return out; // Table 7-5: all 16, and only the first 16 entries are used.
    }
    let raster = if matrix_id < 3 { &DEFAULT_INTRA_RASTER } else { &DEFAULT_INTER_RASTER };
    for (i, (x, y)) in diag_scan_order(8).into_iter().enumerate() {
        out[i] = raster[y * 8 + x];
    }
    out
}

/// `ScalingList[sizeId][matrixId][i]` plus the separate DC coefficients for sizeIds 2 and 3.
///
/// **The index `i` is the SIGNALLED order — the up-right diagonal scan — not raster.** That is the
/// order H.265 7.3.4 emits the coefficients in, the order the H.265 `ScalingFactor` derivation
/// consumes them in, and (the part that took reading rather than reasoning) the order
/// `DXVA_Qmatrix_HEVC` wants them in. ffmpeg stores its own copy in raster order and converts BACK
/// through the diagonal scan tables in `ff_dxva2_hevc_fill_scaling_lists`, which is how that
/// direction is known. Falcon skips the round trip: signalled order in, signalled order out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingLists {
    /// `[sizeId][matrixId][i]`. sizeId 0 uses entries 0..16; the rest use 0..64. sizeId 3 signals
    /// only matrixId 0 and 3, and the others are never read by the DXVA fill.
    pub list: [[[u8; 64]; 6]; 4],
    /// `[sizeId - 2][matrixId]` — the DC coefficient for the 16×16 and 32×32 lists.
    pub dc: [[u8; 6]; 2],
    /// True when the SPS or PPS actually signalled `scaling_list_data()`; false when these are the
    /// spec defaults. The matched fixture pair turns on exactly this bit: IMG_1826 says false,
    /// IMG_3258 says true with 20 explicit matrices.
    pub explicit: bool,
    /// How many of the 24 signalled matrices came from `scaling_list_pred_mode_flag == 1` (an
    /// explicit coefficient list) rather than a prediction/default. Reported, not gated.
    pub explicit_matrices: u32,
}

impl Default for ScalingLists {
    /// The H.265 default lists — what applies when `scaling_list_enabled_flag` is 1 but neither the
    /// SPS nor the PPS carries `scaling_list_data()`. **NOT flat 16s**, which is the trap: a decoder
    /// that "skips the quantisation matrices" on a default-list file is not omitting a no-op.
    fn default() -> Self {
        let mut list = [[[16u8; 64]; 6]; 4];
        for (size_id, per_size) in list.iter_mut().enumerate() {
            for (matrix_id, m) in per_size.iter_mut().enumerate() {
                *m = default_list(size_id, matrix_id);
            }
        }
        ScalingLists { list, dc: [[16u8; 6]; 2], explicit: false, explicit_matrices: 0 }
    }
}

/// H.265 7.3.4 `scaling_list_data()`.
fn parse_scaling_list_data(r: &mut BitReader) -> Option<ScalingLists> {
    let mut sl = ScalingLists { explicit: true, ..ScalingLists::default() };
    let mut explicit_matrices = 0u32;
    for size_id in 0..4usize {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0usize;
        while matrix_id < 6 {
            let pred_mode = r.flag()?;
            if !pred_mode {
                let delta = r.ue()? as usize;
                if delta == 0 {
                    sl.list[size_id][matrix_id] = default_list(size_id, matrix_id);
                    if size_id > 1 {
                        sl.dc[size_id - 2][matrix_id] = 16;
                    }
                } else {
                    let back = delta.checked_mul(step)?;
                    let refm = matrix_id.checked_sub(back)?;
                    sl.list[size_id][matrix_id] = sl.list[size_id][refm];
                    if size_id > 1 {
                        sl.dc[size_id - 2][matrix_id] = sl.dc[size_id - 2][refm];
                    }
                }
            } else {
                explicit_matrices += 1;
                let coef_num = 64.min(1usize << (4 + (size_id << 1)));
                let mut next = 8i32;
                if size_id > 1 {
                    let dc_minus8 = r.se()?;
                    if !(-7..=247).contains(&dc_minus8) {
                        return None;
                    }
                    next = dc_minus8 + 8;
                    sl.dc[size_id - 2][matrix_id] = next as u8;
                }
                for i in 0..coef_num {
                    let d = r.se()?;
                    // v0.8.152 (R3-L6): `wrapping_add`, not `+`. `d` is a `se(v)` and this parser's
                    // `ue()` can legitimately return `u32::MAX` from a 32-zero prefix, which `se()`
                    // maps to `i32::MIN`/`i32::MAX` — and `next` is 1..=255 here, so `1 + i32::MAX`
                    // OVERFLOWS. The workspace leaves overflow-checks ON for first-party crates
                    // (`[profile.dev.package."*"]` disables them for DEPENDENCIES only), so a
                    // crafted parameter set PANICKED under `cargo test` in a module whose header
                    // promises "an `Err`, never a panic". Release already wrapped; this makes debug
                    // agree with it, and `rem_euclid` lands the result back in 0..=255 either way,
                    // so no real file's coefficient changes by one bit.
                    next = next.wrapping_add(d).rem_euclid(256);
                    sl.list[size_id][matrix_id][i] = next as u8;
                }
            }
            matrix_id += step;
        }
    }
    sl.explicit_matrices = explicit_matrices;
    Some(sl)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// SPS
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The VUI fields E2 pinned the colour story on. Parsed here so the two stages read the same bytes
/// and can be checked against each other, never so that this stage decides colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpsVui {
    pub present: bool,
    pub video_full_range_flag: bool,
    pub colour_description_present: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coeffs: u8,
    pub chroma_loc_info_present: bool,
    pub chroma_sample_loc_type_top_field: u32,
}

/// Everything `DXVA_PicParams_HEVC` needs from the sequence parameter set, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sps {
    pub sps_id: u32,
    pub max_sub_layers_minus1: u32,
    pub profile_idc: u8,
    pub level_idc: u8,
    pub general_tier_flag: bool,
    pub chroma_format_idc: u32,
    pub separate_colour_plane_flag: bool,
    pub width: u32,
    pub height: u32,
    pub conf_win: Option<(u32, u32, u32, u32)>,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
    pub log2_max_poc_lsb_minus4: u32,
    /// `sps_max_dec_pic_buffering_minus1` of the HIGHEST temporal sub-layer — the one DXVA wants.
    pub max_dec_pic_buffering_minus1: u32,
    pub max_num_reorder_pics: u32,
    pub log2_min_cb_size_minus3: u32,
    pub log2_diff_max_min_cb_size: u32,
    pub log2_min_tb_size_minus2: u32,
    pub log2_diff_max_min_tb_size: u32,
    pub max_transform_hierarchy_depth_inter: u32,
    pub max_transform_hierarchy_depth_intra: u32,
    pub scaling_list_enabled_flag: bool,
    pub sps_scaling_list_data_present_flag: bool,
    pub scaling_lists: ScalingLists,
    pub amp_enabled_flag: bool,
    pub sao_enabled_flag: bool,
    pub pcm_enabled_flag: bool,
    pub pcm_sample_bit_depth_luma_minus1: u32,
    pub pcm_sample_bit_depth_chroma_minus1: u32,
    pub log2_min_pcm_cb_size_minus3: u32,
    pub log2_diff_max_min_pcm_cb_size: u32,
    pub pcm_loop_filter_disabled_flag: bool,
    pub num_short_term_ref_pic_sets: u32,
    pub long_term_ref_pics_present_flag: bool,
    pub num_long_term_ref_pics_sps: u32,
    pub temporal_mvp_enabled_flag: bool,
    pub strong_intra_smoothing_enabled_flag: bool,
    pub vui: SpsVui,
    /// True when the parse reached `rbsp_trailing_bits()` exactly. See the module header.
    pub tail_verified: bool,
}

impl Sps {
    /// `MinCbSizeY` — the minimum luma coding block edge, in samples.
    pub fn min_cb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3)
    }
    /// `PicWidthInMinCbsY`.
    pub fn min_cb_width(&self) -> u32 {
        self.width / self.min_cb_size()
    }
    /// `PicHeightInMinCbsY`.
    pub fn min_cb_height(&self) -> u32 {
        self.height / self.min_cb_size()
    }
}

/// H.265 7.3.3 `profile_tier_level()`. Returns `(profile_idc, level_idc, tier_flag)`.
fn parse_ptl(r: &mut BitReader, max_sub_layers_minus1: u32) -> Option<(u8, u8, bool)> {
    let _profile_space = r.u(2)?;
    let tier = r.flag()?;
    let profile_idc = r.u(5)? as u8;
    for _ in 0..32 {
        r.u(1)?;
    }
    // progressive/interlaced/non_packed/frame_only + 43 reserved/constraint bits + inbld.
    r.u(4)?;
    r.u(32)?;
    r.u(11)?;
    r.u(1)?;
    let level_idc = r.u(8)? as u8;
    let mut sub_profile = Vec::new();
    let mut sub_level = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        sub_profile.push(r.flag()?);
        sub_level.push(r.flag()?);
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.u(2)?;
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if sub_profile[i] {
            r.u(2)?;
            r.u(1)?;
            r.u(5)?;
            for _ in 0..32 {
                r.u(1)?;
            }
            r.u(4)?;
            r.u(32)?;
            r.u(11)?;
            r.u(1)?;
        }
        if sub_level[i] {
            r.u(8)?;
        }
    }
    Some((profile_idc, level_idc, tier))
}

/// H.265 7.3.7 `st_ref_pic_set()`. Returns `NumDeltaPocs` for the set.
///
/// The `inter_ref_pic_set_prediction_flag` branch is REFUSED rather than approximated (see the
/// module header): deriving `NumDeltaPocs` through 7.4.8 for a set predicted from another set is
/// real work, no still-picture HEIC in the corpus signals a single short-term set at all, and a
/// desynchronised SPS parse is exactly the silent-wrong-pixels failure this round is retiring.
fn parse_st_ref_pic_set(r: &mut BitReader, idx: u32) -> Option<Result<u32, ()>> {
    if idx != 0 && r.flag()? {
        return Some(Err(()));
    }
    let num_neg = r.ue()?;
    let num_pos = r.ue()?;
    if num_neg > 16 || num_pos > 16 {
        return None;
    }
    for _ in 0..num_neg {
        r.ue()?;
        r.u(1)?;
    }
    for _ in 0..num_pos {
        r.ue()?;
        r.u(1)?;
    }
    Some(Ok(num_neg + num_pos))
}

/// H.265 E.2.2 `hrd_parameters()` — parsed only so that [`parse_sps`]'s trailing-bit self-check
/// still means something on a stream that carries timing information.
fn parse_hrd(r: &mut BitReader, common_inf: bool, max_sub_layers_minus1: u32) -> Option<()> {
    let mut nal_hrd = false;
    let mut vcl_hrd = false;
    let mut sub_pic = false;
    if common_inf {
        nal_hrd = r.flag()?;
        vcl_hrd = r.flag()?;
        if nal_hrd || vcl_hrd {
            sub_pic = r.flag()?;
            if sub_pic {
                r.u(8)?;
                r.u(5)?;
                r.u(1)?;
                r.u(5)?;
            }
            r.u(4)?;
            r.u(4)?;
            if sub_pic {
                r.u(4)?;
            }
            r.u(5)?;
            r.u(5)?;
            r.u(5)?;
        }
    }
    for _ in 0..=max_sub_layers_minus1 {
        let fixed_general = r.flag()?;
        let fixed_within_cvs = if !fixed_general { r.flag()? } else { true };
        let mut low_delay = false;
        if fixed_within_cvs {
            r.ue()?;
        } else {
            low_delay = r.flag()?;
        }
        let cpb_cnt_minus1 = if !low_delay { r.ue()? } else { 0 };
        if cpb_cnt_minus1 > 31 {
            return None;
        }
        for present in [nal_hrd, vcl_hrd] {
            if !present {
                continue;
            }
            for _ in 0..=cpb_cnt_minus1 {
                r.ue()?;
                r.ue()?;
                if sub_pic {
                    r.ue()?;
                    r.ue()?;
                }
                r.u(1)?;
            }
        }
    }
    Some(())
}

/// H.265 E.2.1 `vui_parameters()`.
fn parse_vui(r: &mut BitReader, max_sub_layers_minus1: u32) -> Option<SpsVui> {
    let mut v = SpsVui { present: true, ..SpsVui::default() };
    if r.flag()? {
        // aspect_ratio_info_present_flag
        let idc = r.u(8)?;
        if idc == 255 {
            r.u(16)?;
            r.u(16)?;
        }
    }
    if r.flag()? {
        r.u(1)?; // overscan_appropriate_flag
    }
    if r.flag()? {
        // video_signal_type_present_flag
        r.u(3)?; // video_format
        v.video_full_range_flag = r.flag()?;
        if r.flag()? {
            v.colour_description_present = true;
            v.colour_primaries = r.u(8)? as u8;
            v.transfer_characteristics = r.u(8)? as u8;
            v.matrix_coeffs = r.u(8)? as u8;
        }
    }
    if r.flag()? {
        v.chroma_loc_info_present = true;
        v.chroma_sample_loc_type_top_field = r.ue()?;
        r.ue()?; // bottom field
    }
    r.u(1)?; // neutral_chroma_indication_flag
    r.u(1)?; // field_seq_flag
    r.u(1)?; // frame_field_info_present_flag
    if r.flag()? {
        // default_display_window_flag
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    if r.flag()? {
        // vui_timing_info_present_flag
        r.u(32)?;
        r.u(32)?;
        if r.flag()? {
            r.ue()?; // vui_num_ticks_poc_diff_one_minus1
        }
        if r.flag()? {
            parse_hrd(r, true, max_sub_layers_minus1)?;
        }
    }
    if r.flag()? {
        // bitstream_restriction_flag
        r.u(1)?;
        r.u(1)?;
        r.u(1)?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    Some(v)
}

/// H.265 7.3.2.2 — the sequence parameter set, from a NAL that still carries its 2-byte header.
pub fn parse_sps(nal: &[u8]) -> Result<Sps, HwDecError> {
    if nal.len() < 3 {
        return Err(HwDecError::Bitstream("SPS NAL shorter than its own header"));
    }
    let rbsp = unescape_rbsp(&nal[2..]);
    let r = &mut BitReader::new(&rbsp);
    let bad = || HwDecError::Bitstream("SPS ended mid-field");

    let _vps_id = r.u(4).ok_or_else(bad)?;
    let max_sub_layers_minus1 = r.u(3).ok_or_else(bad)?;
    let _nesting = r.u(1).ok_or_else(bad)?;
    let (profile_idc, level_idc, tier) = parse_ptl(r, max_sub_layers_minus1).ok_or_else(bad)?;
    let sps_id = r.ue().ok_or_else(bad)?;
    let chroma_format_idc = r.ue().ok_or_else(bad)?;
    let separate_colour_plane_flag =
        if chroma_format_idc == 3 { r.flag().ok_or_else(bad)? } else { false };
    if separate_colour_plane_flag {
        return Err(HwDecError::Unsupported("separate_colour_plane_flag"));
    }
    let width = r.ue().ok_or_else(bad)?;
    let height = r.ue().ok_or_else(bad)?;
    let conf_win = if r.flag().ok_or_else(bad)? {
        Some((
            r.ue().ok_or_else(bad)?,
            r.ue().ok_or_else(bad)?,
            r.ue().ok_or_else(bad)?,
            r.ue().ok_or_else(bad)?,
        ))
    } else {
        None
    };
    let bit_depth_luma_minus8 = r.ue().ok_or_else(bad)?;
    let bit_depth_chroma_minus8 = r.ue().ok_or_else(bad)?;
    let log2_max_poc_lsb_minus4 = r.ue().ok_or_else(bad)?;
    // v0.8.152 (R3-L6): H.265 7.4.3.2 bounds this field to 0..=12, and the check was simply absent
    // — contrast the block-size exponents a dozen lines below, which ARE range-checked with the
    // note "a crafted SPS could claim a 4 GB coding block". It feeds `r.u(log2_max_poc_lsb_minus4
    // + 4)` in the long-term-ref-pic loop: an unbounded `ue()` overflowed that add (debug panic),
    // and in release `u32::MAX + 4` wrapped to 3, making `u(3)` a perfectly legal read from which
    // the SPS parse silently DESYNCHRONISES. Refuse the field instead — and note that the desync
    // is exactly what `tail_verified` catches, which is why the two ship in one change.
    if log2_max_poc_lsb_minus4 > 12 {
        return Err(HwDecError::Bitstream("SPS log2_max_poc_lsb_minus4 out of range"));
    }
    let sub_layer_ordering_info = r.flag().ok_or_else(bad)?;
    let first = if sub_layer_ordering_info { 0 } else { max_sub_layers_minus1 };
    let mut max_dec_pic_buffering_minus1 = 0;
    let mut max_num_reorder_pics = 0;
    for _ in first..=max_sub_layers_minus1 {
        max_dec_pic_buffering_minus1 = r.ue().ok_or_else(bad)?;
        max_num_reorder_pics = r.ue().ok_or_else(bad)?;
        r.ue().ok_or_else(bad)?; // max_latency_increase_plus1
    }
    let log2_min_cb_size_minus3 = r.ue().ok_or_else(bad)?;
    let log2_diff_max_min_cb_size = r.ue().ok_or_else(bad)?;
    let log2_min_tb_size_minus2 = r.ue().ok_or_else(bad)?;
    let log2_diff_max_min_tb_size = r.ue().ok_or_else(bad)?;
    let max_transform_hierarchy_depth_inter = r.ue().ok_or_else(bad)?;
    let max_transform_hierarchy_depth_intra = r.ue().ok_or_else(bad)?;
    // A crafted SPS could claim a 4 GB coding block; every one of these feeds a shift.
    if log2_min_cb_size_minus3 > 3
        || log2_diff_max_min_cb_size > 3
        || log2_min_tb_size_minus2 > 3
        || log2_diff_max_min_tb_size > 3
    {
        return Err(HwDecError::Bitstream("SPS block-size exponents out of range"));
    }

    let scaling_list_enabled_flag = r.flag().ok_or_else(bad)?;
    let mut sps_scaling_list_data_present_flag = false;
    let mut scaling_lists = ScalingLists::default();
    if scaling_list_enabled_flag {
        sps_scaling_list_data_present_flag = r.flag().ok_or_else(bad)?;
        if sps_scaling_list_data_present_flag {
            scaling_lists = parse_scaling_list_data(r)
                .ok_or(HwDecError::Bitstream("SPS scaling_list_data ended mid-field"))?;
        }
    }
    let amp_enabled_flag = r.flag().ok_or_else(bad)?;
    let sao_enabled_flag = r.flag().ok_or_else(bad)?;
    let pcm_enabled_flag = r.flag().ok_or_else(bad)?;
    let (mut pcm_l, mut pcm_c, mut pcm_min, mut pcm_diff, mut pcm_lf) = (0, 0, 0, 0, false);
    if pcm_enabled_flag {
        pcm_l = r.u(4).ok_or_else(bad)?;
        pcm_c = r.u(4).ok_or_else(bad)?;
        pcm_min = r.ue().ok_or_else(bad)?;
        pcm_diff = r.ue().ok_or_else(bad)?;
        pcm_lf = r.flag().ok_or_else(bad)?;
    }
    let num_short_term_ref_pic_sets = r.ue().ok_or_else(bad)?;
    if num_short_term_ref_pic_sets > 64 {
        return Err(HwDecError::Bitstream("num_short_term_ref_pic_sets > 64"));
    }
    for i in 0..num_short_term_ref_pic_sets {
        match parse_st_ref_pic_set(r, i).ok_or_else(bad)? {
            Ok(_) => {}
            Err(()) => return Err(HwDecError::Unsupported("inter_ref_pic_set_prediction_flag")),
        }
    }
    let long_term_ref_pics_present_flag = r.flag().ok_or_else(bad)?;
    let mut num_long_term_ref_pics_sps = 0;
    if long_term_ref_pics_present_flag {
        num_long_term_ref_pics_sps = r.ue().ok_or_else(bad)?;
        if num_long_term_ref_pics_sps > 32 {
            return Err(HwDecError::Bitstream("num_long_term_ref_pics_sps > 32"));
        }
        for _ in 0..num_long_term_ref_pics_sps {
            r.u(log2_max_poc_lsb_minus4 + 4).ok_or_else(bad)?;
            r.u(1).ok_or_else(bad)?;
        }
    }
    let temporal_mvp_enabled_flag = r.flag().ok_or_else(bad)?;
    let strong_intra_smoothing_enabled_flag = r.flag().ok_or_else(bad)?;
    let mut vui = SpsVui::default();
    if r.flag().ok_or_else(bad)? {
        vui = parse_vui(r, max_sub_layers_minus1).ok_or_else(bad)?;
    }
    // sps_extension_present_flag — any extension means a syntax this stage does not model, and
    // HEVC_VLD_MAIN would not accept the stream anyway.
    if r.flag().ok_or_else(bad)? {
        return Err(HwDecError::Unsupported("sps_extension_present_flag"));
    }
    let tail_verified = r.trailing_bits_ok();

    Ok(Sps {
        sps_id,
        max_sub_layers_minus1,
        profile_idc,
        level_idc,
        general_tier_flag: tier,
        chroma_format_idc,
        separate_colour_plane_flag,
        width,
        height,
        conf_win,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1,
        max_num_reorder_pics,
        log2_min_cb_size_minus3,
        log2_diff_max_min_cb_size,
        log2_min_tb_size_minus2,
        log2_diff_max_min_tb_size,
        max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra,
        scaling_list_enabled_flag,
        sps_scaling_list_data_present_flag,
        scaling_lists,
        amp_enabled_flag,
        sao_enabled_flag,
        pcm_enabled_flag,
        pcm_sample_bit_depth_luma_minus1: pcm_l,
        pcm_sample_bit_depth_chroma_minus1: pcm_c,
        log2_min_pcm_cb_size_minus3: pcm_min,
        log2_diff_max_min_pcm_cb_size: pcm_diff,
        pcm_loop_filter_disabled_flag: pcm_lf,
        num_short_term_ref_pic_sets,
        long_term_ref_pics_present_flag,
        num_long_term_ref_pics_sps,
        temporal_mvp_enabled_flag,
        strong_intra_smoothing_enabled_flag,
        vui,
        tail_verified,
    })
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// PPS
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Everything `DXVA_PicParams_HEVC` needs from the picture parameter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pps {
    pub pps_id: u32,
    pub sps_id: u32,
    pub dependent_slice_segments_enabled_flag: bool,
    pub output_flag_present_flag: bool,
    pub num_extra_slice_header_bits: u32,
    pub sign_data_hiding_enabled_flag: bool,
    pub cabac_init_present_flag: bool,
    pub num_ref_idx_l0_default_active_minus1: u32,
    pub num_ref_idx_l1_default_active_minus1: u32,
    pub init_qp_minus26: i32,
    pub constrained_intra_pred_flag: bool,
    pub transform_skip_enabled_flag: bool,
    pub cu_qp_delta_enabled_flag: bool,
    pub diff_cu_qp_delta_depth: u32,
    pub pps_cb_qp_offset: i32,
    pub pps_cr_qp_offset: i32,
    pub pps_slice_chroma_qp_offsets_present_flag: bool,
    pub weighted_pred_flag: bool,
    pub weighted_bipred_flag: bool,
    pub transquant_bypass_enabled_flag: bool,
    pub tiles_enabled_flag: bool,
    pub entropy_coding_sync_enabled_flag: bool,
    pub num_tile_columns_minus1: u32,
    pub num_tile_rows_minus1: u32,
    pub uniform_spacing_flag: bool,
    pub column_width_minus1: Vec<u32>,
    pub row_height_minus1: Vec<u32>,
    pub loop_filter_across_tiles_enabled_flag: bool,
    pub pps_loop_filter_across_slices_enabled_flag: bool,
    pub deblocking_filter_override_enabled_flag: bool,
    pub pps_deblocking_filter_disabled_flag: bool,
    pub pps_beta_offset_div2: i32,
    pub pps_tc_offset_div2: i32,
    pub pps_scaling_list_data_present_flag: bool,
    pub scaling_lists: Option<ScalingLists>,
    pub lists_modification_present_flag: bool,
    pub log2_parallel_merge_level_minus2: u32,
    pub slice_segment_header_extension_present_flag: bool,
    pub tail_verified: bool,
}

/// H.265 7.3.2.3 — the picture parameter set.
pub fn parse_pps(nal: &[u8]) -> Result<Pps, HwDecError> {
    if nal.len() < 3 {
        return Err(HwDecError::Bitstream("PPS NAL shorter than its own header"));
    }
    let rbsp = unescape_rbsp(&nal[2..]);
    let r = &mut BitReader::new(&rbsp);
    let bad = || HwDecError::Bitstream("PPS ended mid-field");

    let pps_id = r.ue().ok_or_else(bad)?;
    let sps_id = r.ue().ok_or_else(bad)?;
    let dependent_slice_segments_enabled_flag = r.flag().ok_or_else(bad)?;
    let output_flag_present_flag = r.flag().ok_or_else(bad)?;
    let num_extra_slice_header_bits = r.u(3).ok_or_else(bad)?;
    let sign_data_hiding_enabled_flag = r.flag().ok_or_else(bad)?;
    let cabac_init_present_flag = r.flag().ok_or_else(bad)?;
    let num_ref_idx_l0_default_active_minus1 = r.ue().ok_or_else(bad)?;
    let num_ref_idx_l1_default_active_minus1 = r.ue().ok_or_else(bad)?;
    let init_qp_minus26 = r.se().ok_or_else(bad)?;
    let constrained_intra_pred_flag = r.flag().ok_or_else(bad)?;
    let transform_skip_enabled_flag = r.flag().ok_or_else(bad)?;
    let cu_qp_delta_enabled_flag = r.flag().ok_or_else(bad)?;
    let diff_cu_qp_delta_depth =
        if cu_qp_delta_enabled_flag { r.ue().ok_or_else(bad)? } else { 0 };
    let pps_cb_qp_offset = r.se().ok_or_else(bad)?;
    let pps_cr_qp_offset = r.se().ok_or_else(bad)?;
    let pps_slice_chroma_qp_offsets_present_flag = r.flag().ok_or_else(bad)?;
    let weighted_pred_flag = r.flag().ok_or_else(bad)?;
    let weighted_bipred_flag = r.flag().ok_or_else(bad)?;
    let transquant_bypass_enabled_flag = r.flag().ok_or_else(bad)?;
    let tiles_enabled_flag = r.flag().ok_or_else(bad)?;
    let entropy_coding_sync_enabled_flag = r.flag().ok_or_else(bad)?;
    let mut num_tile_columns_minus1 = 0;
    let mut num_tile_rows_minus1 = 0;
    let mut uniform_spacing_flag = true;
    let mut column_width_minus1 = Vec::new();
    let mut row_height_minus1 = Vec::new();
    let mut loop_filter_across_tiles_enabled_flag = true;
    if tiles_enabled_flag {
        num_tile_columns_minus1 = r.ue().ok_or_else(bad)?;
        num_tile_rows_minus1 = r.ue().ok_or_else(bad)?;
        // DXVA's arrays are 19 columns and 21 rows; anything past that is not marshallable.
        if num_tile_columns_minus1 >= 19 || num_tile_rows_minus1 >= 21 {
            return Err(HwDecError::Unsupported("more tiles than DXVA_PicParams_HEVC can carry"));
        }
        uniform_spacing_flag = r.flag().ok_or_else(bad)?;
        if !uniform_spacing_flag {
            for _ in 0..num_tile_columns_minus1 {
                column_width_minus1.push(r.ue().ok_or_else(bad)?);
            }
            for _ in 0..num_tile_rows_minus1 {
                row_height_minus1.push(r.ue().ok_or_else(bad)?);
            }
        }
        loop_filter_across_tiles_enabled_flag = r.flag().ok_or_else(bad)?;
    }
    let pps_loop_filter_across_slices_enabled_flag = r.flag().ok_or_else(bad)?;
    let mut deblocking_filter_override_enabled_flag = false;
    let mut pps_deblocking_filter_disabled_flag = false;
    let mut pps_beta_offset_div2 = 0;
    let mut pps_tc_offset_div2 = 0;
    if r.flag().ok_or_else(bad)? {
        // deblocking_filter_control_present_flag
        deblocking_filter_override_enabled_flag = r.flag().ok_or_else(bad)?;
        pps_deblocking_filter_disabled_flag = r.flag().ok_or_else(bad)?;
        if !pps_deblocking_filter_disabled_flag {
            pps_beta_offset_div2 = r.se().ok_or_else(bad)?;
            pps_tc_offset_div2 = r.se().ok_or_else(bad)?;
        }
    }
    let pps_scaling_list_data_present_flag = r.flag().ok_or_else(bad)?;
    let scaling_lists = if pps_scaling_list_data_present_flag {
        Some(
            parse_scaling_list_data(r)
                .ok_or(HwDecError::Bitstream("PPS scaling_list_data ended mid-field"))?,
        )
    } else {
        None
    };
    let lists_modification_present_flag = r.flag().ok_or_else(bad)?;
    let log2_parallel_merge_level_minus2 = r.ue().ok_or_else(bad)?;
    let slice_segment_header_extension_present_flag = r.flag().ok_or_else(bad)?;
    if r.flag().ok_or_else(bad)? {
        return Err(HwDecError::Unsupported("pps_extension_present_flag"));
    }
    let tail_verified = r.trailing_bits_ok();

    Ok(Pps {
        pps_id,
        sps_id,
        dependent_slice_segments_enabled_flag,
        output_flag_present_flag,
        num_extra_slice_header_bits,
        sign_data_hiding_enabled_flag,
        cabac_init_present_flag,
        num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1,
        init_qp_minus26,
        constrained_intra_pred_flag,
        transform_skip_enabled_flag,
        cu_qp_delta_enabled_flag,
        diff_cu_qp_delta_depth,
        pps_cb_qp_offset,
        pps_cr_qp_offset,
        pps_slice_chroma_qp_offsets_present_flag,
        weighted_pred_flag,
        weighted_bipred_flag,
        transquant_bypass_enabled_flag,
        tiles_enabled_flag,
        entropy_coding_sync_enabled_flag,
        num_tile_columns_minus1,
        num_tile_rows_minus1,
        uniform_spacing_flag,
        column_width_minus1,
        row_height_minus1,
        loop_filter_across_tiles_enabled_flag,
        pps_loop_filter_across_slices_enabled_flag,
        deblocking_filter_override_enabled_flag,
        pps_deblocking_filter_disabled_flag,
        pps_beta_offset_div2,
        pps_tc_offset_div2,
        pps_scaling_list_data_present_flag,
        scaling_lists,
        lists_modification_present_flag,
        log2_parallel_merge_level_minus2,
        slice_segment_header_extension_present_flag,
        tail_verified,
    })
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// hvcC — the HEVCDecoderConfigurationRecord
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The parameter sets a HEIC tile decodes against, plus the NAL length prefix width its item data
/// uses. Built once per file from the one `hvcC` E1 reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    pub sps: Sps,
    pub pps: Pps,
    /// `lengthSizeMinusOne + 1` — how many bytes prefix each NAL inside an item's data.
    pub length_size: u8,
    /// The record's own declarations, kept so a mismatch against the SPS is checkable.
    pub record_chroma_format: u8,
    pub record_bit_depth_luma: u8,
    pub record_bit_depth_chroma: u8,
    pub record_profile_idc: u8,
    pub record_level_idc: u8,
    /// Every NAL the record carried, as `(nal_type, bytes)` — VPS included, unparsed.
    pub nals: Vec<(u8, Vec<u8>)>,
}

/// Parse an `hvcC` box BODY (ISO/IEC 14496-15 HEVCDecoderConfigurationRecord).
pub fn parse_hvcc(body: &[u8]) -> Result<ParameterSets, HwDecError> {
    let short = || HwDecError::Bitstream("hvcC record truncated");
    if body.len() < 23 {
        return Err(short());
    }
    if body[0] != 1 {
        return Err(HwDecError::Unsupported("hvcC configurationVersion != 1"));
    }
    let record_profile_idc = body[1] & 0x1f;
    let record_level_idc = body[12];
    let record_chroma_format = body[16] & 0x03;
    let record_bit_depth_luma = (body[17] & 0x07) + 8;
    let record_bit_depth_chroma = (body[18] & 0x07) + 8;
    let length_size = (body[21] & 0x03) + 1;
    let num_arrays = body[22];

    let mut p = 23usize;
    let mut nals: Vec<(u8, Vec<u8>)> = Vec::new();
    for _ in 0..num_arrays {
        if p + 3 > body.len() {
            return Err(short());
        }
        let nal_type = body[p] & 0x3f;
        let count = u16::from_be_bytes([body[p + 1], body[p + 2]]) as usize;
        p += 3;
        for _ in 0..count {
            if p + 2 > body.len() {
                return Err(short());
            }
            let len = u16::from_be_bytes([body[p], body[p + 1]]) as usize;
            p += 2;
            let end = p.checked_add(len).ok_or_else(short)?;
            if end > body.len() {
                return Err(short());
            }
            nals.push((nal_type, body[p..end].to_vec()));
            p = end;
        }
    }

    let sps_nal = nals
        .iter()
        .find(|(t, _)| *t == 33)
        .ok_or(HwDecError::Bitstream("hvcC carries no SPS"))?;
    let pps_nal = nals
        .iter()
        .find(|(t, _)| *t == 34)
        .ok_or(HwDecError::Bitstream("hvcC carries no PPS"))?;
    let sps = parse_sps(&sps_nal.1)?;
    let pps = parse_pps(&pps_nal.1)?;
    if pps.sps_id != sps.sps_id {
        return Err(HwDecError::Bitstream("PPS names an SPS the record does not carry"));
    }

    Ok(ParameterSets {
        sps,
        pps,
        length_size,
        record_chroma_format,
        record_bit_depth_luma,
        record_bit_depth_chroma,
        record_profile_idc,
        record_level_idc,
        nals,
    })
}

impl ParameterSets {
    /// The scaling lists that actually apply: the PPS's if it carries any, otherwise the SPS's
    /// (which are the H.265 DEFAULTS when the SPS did not signal `scaling_list_data()`).
    pub fn effective_scaling_lists(&self) -> &ScalingLists {
        self.pps.scaling_lists.as_ref().unwrap_or(&self.sps.scaling_lists)
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Item data → slice NALs
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One NAL unit inside a tile item, as a range of the item's own bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemNal {
    pub nal_type: u8,
    pub start: usize,
    pub len: usize,
}

impl ItemNal {
    /// VCL NAL types are 0..=31; those are the slice segments DXVA wants in the bitstream buffer.
    pub fn is_vcl(&self) -> bool {
        self.nal_type <= 31
    }
}

/// Split a tile item's length-prefixed NAL stream. Refuses rather than truncates: a length that
/// runs past the end of the item is an `Err`, because a decoder that silently drops the tail of a
/// slice produces a picture, and the picture is wrong.
pub fn split_item_nals(item: &[u8], length_size: u8) -> Result<Vec<ItemNal>, HwDecError> {
    if !(1..=4).contains(&length_size) {
        return Err(HwDecError::Bitstream("hvcC lengthSizeMinusOne out of range"));
    }
    let ls = length_size as usize;
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + ls <= item.len() {
        let mut len = 0usize;
        for i in 0..ls {
            len = (len << 8) | item[p + i] as usize;
        }
        p += ls;
        if len < 2 {
            return Err(HwDecError::Bitstream("item NAL shorter than its own header"));
        }
        let end = p.checked_add(len).ok_or(HwDecError::Bitstream("item NAL length overflows"))?;
        if end > item.len() {
            return Err(HwDecError::Bitstream("item NAL runs past the end of the item"));
        }
        out.push(ItemNal { nal_type: (item[p] >> 1) & 0x3f, start: p, len });
        p = end;
    }
    if p != item.len() {
        return Err(HwDecError::Bitstream("item has trailing bytes after its last NAL"));
    }
    if out.is_empty() {
        return Err(HwDecError::Bitstream("item carries no NAL units"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diagonal_scan_is_the_one_the_spec_derives() {
        // H.265 6.5.3, first two diagonals of the 4x4 and 8x8 scans.
        let s4 = diag_scan_order(4);
        assert_eq!(s4.len(), 16);
        assert_eq!(&s4[..6], &[(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(s4[15], (3, 3));
        let s8 = diag_scan_order(8);
        assert_eq!(s8.len(), 64);
        assert_eq!(&s8[..6], &[(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(s8[63], (7, 7));
        // Every position exactly once — the property that makes the raster↔diagonal map a bijection.
        let mut seen = vec![false; 64];
        for (x, y) in s8 {
            assert!(!seen[y * 8 + x], "the scan visited ({x},{y}) twice");
            seen[y * 8 + x] = true;
        }
        assert!(seen.into_iter().all(|s| s));
    }

    #[test]
    fn the_default_lists_are_the_specs_table_7_6_in_signalled_order() {
        // The published Table 7-6 sequence for ScalingList[1..3][0..2][i] (intra), the first 16
        // entries — ten 16s then the alternating 17/16 the diagonal produces from the raster matrix.
        let intra = default_list(1, 0);
        assert_eq!(
            &intra[..16],
            &[16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18]
        );
        assert_eq!(intra[63], 115, "the last diagonal entry is the raster corner");
        let inter = default_list(1, 3);
        assert_eq!(
            &inter[..16],
            &[16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18]
        );
        assert_eq!(inter[63], 91);
        // sizeId 0 is flat 16 (Table 7-5) — and that is the ONLY flat default.
        assert_eq!(default_list(0, 0), [16u8; 64]);
        assert_ne!(&default_list(2, 0)[..], &[16u8; 64][..], "the 16x16 default is NOT flat");
    }

    #[test]
    fn the_bit_reader_refuses_rather_than_running_off_the_end() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(r.u(8), Some(0xff));
        assert_eq!(r.u(1), None);
        // A ue(v) whose prefix never terminates.
        let mut r = BitReader::new(&[0, 0, 0, 0, 0, 0]);
        assert_eq!(r.ue(), None);
        // se(v) mapping, both signs: the bits are `010` `011` then padding.
        let mut r = BitReader::new(&[0b0100_1100]);
        assert_eq!(r.se(), Some(1)); // 010 -> ue 1 -> se +1
        assert_eq!(r.se(), Some(-1)); // 011 -> ue 2 -> se -1
        // ue(v) at the boundaries of the codes above it.
        let mut r = BitReader::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        assert_eq!(r.ue(), Some(0)); // 1
        assert_eq!(r.ue(), Some(1)); // 010
        assert_eq!(r.ue(), Some(2)); // 011
        assert_eq!(r.ue(), Some(3)); // 00100
        assert_eq!(r.ue(), Some(4)); // 00101
    }

    #[test]
    fn unescaping_removes_only_the_emulation_bytes() {
        assert_eq!(unescape_rbsp(&[0, 0, 3, 1]), vec![0, 0, 1]);
        assert_eq!(unescape_rbsp(&[0, 0, 3, 0, 0, 3, 2]), vec![0, 0, 0, 0, 2]);
        // 00 03 is NOT an escape (needs two zeros), and a lone 03 survives.
        assert_eq!(unescape_rbsp(&[0, 3, 4]), vec![0, 3, 4]);
    }

    #[test]
    fn item_splitting_fails_closed_on_every_malformed_shape() {
        // one 2-byte NAL, 4-byte lengths
        let ok = [0, 0, 0, 2, 0x28, 0x01];
        assert_eq!(split_item_nals(&ok, 4).unwrap().len(), 1);
        // length runs past the end
        assert!(split_item_nals(&[0, 0, 0, 9, 0x28, 0x01], 4).is_err());
        // trailing garbage after the last NAL
        assert!(split_item_nals(&[0, 0, 0, 2, 0x28, 0x01, 0x00], 4).is_err());
        // a zero-length NAL
        assert!(split_item_nals(&[0, 0, 0, 0], 4).is_err());
        // empty item
        assert!(split_item_nals(&[], 4).is_err());
        // an impossible length_size
        assert!(split_item_nals(&ok, 0).is_err());
        assert!(split_item_nals(&ok, 5).is_err());
    }

    /// A minimal MSB-first bit WRITER, so the hostile parameter sets below are spelled in the
    /// syntax's own terms rather than in hand-computed hexadecimal. It is deliberately able to emit
    /// codes a conforming encoder never would — [`Self::ue_with_zeros`] takes the prefix length as
    /// an argument, which is the only way to write the 32-zero `ue(v)` R3-L6 measured.
    struct Bits {
        v: Vec<u8>,
        n: usize,
    }
    impl Bits {
        fn new() -> Self {
            Bits { v: Vec::new(), n: 0 }
        }
        fn bit(&mut self, b: u8) {
            if self.n.is_multiple_of(8) {
                self.v.push(0);
            }
            if b != 0 {
                let i = self.v.len() - 1;
                self.v[i] |= 1 << (7 - (self.n % 8));
            }
            self.n += 1;
        }
        fn u(&mut self, n: u32, val: u64) {
            for k in (0..n).rev() {
                self.bit(((val >> k) & 1) as u8);
            }
        }
        /// `ue(v)` with the zero-prefix length chosen by the caller: `zeros` zeros, the terminating
        /// one, then `zeros` payload bits. The decoded value is `(1 << zeros) - 1 + rest`.
        fn ue_with_zeros(&mut self, zeros: u32, rest: u64) {
            self.u(zeros, 0);
            self.bit(1);
            self.u(zeros, rest);
        }
        /// The canonical (shortest) `ue(v)` for `val`.
        fn ue(&mut self, val: u64) {
            let mut zeros = 0u32;
            while (1u64 << (zeros + 1)) - 1 <= val {
                zeros += 1;
            }
            self.ue_with_zeros(zeros, val - ((1u64 << zeros) - 1));
        }
        fn finish(self) -> Vec<u8> {
            self.v
        }
    }

    /// R3-L6, half one: the two Exp-Golomb codes the finding measured are REACHABLE, so the
    /// arithmetic downstream of them has to survive them. Neither is a value a conforming encoder
    /// emits; both are values this reader can be handed.
    #[test]
    fn the_exp_golomb_extremes_r3_l6_measured_are_reachable() {
        // 32 zeros + terminator + 32 zero payload bits => (1<<32) - 1 = u32::MAX, the largest value
        // `ue()` can answer with (a 33rd zero is refused by the prefix guard).
        let mut b = Bits::new();
        b.ue_with_zeros(32, 0);
        let max = b.finish();
        assert_eq!(BitReader::new(&max).ue(), Some(u32::MAX));
        // `se()` maps that odd k through `k.div_ceil(2) as i32`, which truncates to i32::MIN.
        assert_eq!(BitReader::new(&max).se(), Some(i32::MIN));
        // …and 4_294_967_293 — a 31-zero prefix — truncates to i32::MAX.
        let mut b = Bits::new();
        b.ue_with_zeros(31, 4_294_967_293 - ((1u64 << 31) - 1));
        let hi = b.finish();
        assert_eq!(BitReader::new(&hi).ue(), Some(4_294_967_293));
        assert_eq!(BitReader::new(&hi).se(), Some(i32::MAX));
        // A 33-zero prefix is still refused rather than looping.
        let mut b = Bits::new();
        b.ue_with_zeros(33, 0);
        assert_eq!(BitReader::new(&b.finish()).ue(), None);
    }

    /// R3-L6 site one (`hevc.rs:280`): `next` is 1..=255 and `d` is an `se(v)`, so `next + d` with
    /// `d == i32::MAX` OVERFLOWED — a PANIC under the dev profile, in a module whose header promises
    /// "an `Err`, never a panic and never a silent partial parse". With `wrapping_add` the hostile
    /// coefficient is a decline, which is what every other malformed shape here already is.
    #[test]
    fn a_hostile_scaling_list_coefficient_declines_instead_of_overflowing() {
        let mut b = Bits::new();
        b.bit(1); // sizeId 0, matrixId 0: scaling_list_pred_mode_flag = 1 (explicit coefficients)
        b.ue_with_zeros(31, 4_294_967_293 - ((1u64 << 31) - 1)); // first delta_coef = i32::MAX
        // …and then the bits simply stop, so the SECOND coefficient's `se()?` is the decline.
        let bytes = b.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(parse_scaling_list_data(&mut r), None, "a truncated scaling list declines");
    }

    /// R3-L6 site two (`hevc.rs:662`): `log2_max_poc_lsb_minus4` is an unbounded `ue(v)` that feeds
    /// `r.u(log2_max_poc_lsb_minus4 + 4)`. H.265 7.4.3.2 bounds it to 0..=12 and the check was
    /// absent, so `u32::MAX` panicked on the add in debug and — worse — wrapped to `u(3)` in
    /// release, a legal read from which the whole SPS parse silently desynchronises.
    #[test]
    fn an_out_of_range_poc_lsb_is_refused_by_name() {
        // Everything up to the field under test, then the field.
        let sps_prefix = |poc: &dyn Fn(&mut Bits)| -> Vec<u8> {
            let mut b = Bits::new();
            b.u(4, 0); // sps_video_parameter_set_id
            b.u(3, 0); // sps_max_sub_layers_minus1 = 0 (so the PTL is its 96-bit base form)
            b.u(1, 0); // sps_temporal_id_nesting_flag
            b.u(2, 0); // general_profile_space
            b.u(1, 0); // general_tier_flag
            b.u(5, 1); // general_profile_idc = 1 (Main)
            b.u(32, 0); // the 32 profile-compatibility flags
            b.u(4, 0); // progressive/interlaced/non_packed/frame_only
            b.u(32, 0); // reserved
            b.u(11, 0); // reserved
            b.u(1, 0); // inbld
            b.u(8, 120); // general_level_idc
            b.ue(0); // sps_seq_parameter_set_id
            b.ue(1); // chroma_format_idc = 1 (4:2:0)
            b.ue(64); // pic_width_in_luma_samples
            b.ue(64); // pic_height_in_luma_samples
            b.u(1, 0); // conformance_window_flag
            b.ue(0); // bit_depth_luma_minus8
            b.ue(0); // bit_depth_chroma_minus8
            poc(&mut b); // log2_max_poc_lsb_minus4
            let rbsp = b.finish();
            // The fixture must survive emulation-prevention stripping unchanged, or the bits the
            // parser sees are not the bits this test wrote.
            assert_eq!(unescape_rbsp(&rbsp), rbsp, "the crafted RBSP carries no 00 00 03");
            let mut nal = vec![0x42u8, 0x01]; // NAL header: type 33 (SPS)
            nal.extend_from_slice(&rbsp);
            nal
        };

        // 13 is one past the spec's bound.
        let over = sps_prefix(&|b: &mut Bits| b.ue(13));
        assert_eq!(
            parse_sps(&over),
            Err(HwDecError::Bitstream("SPS log2_max_poc_lsb_minus4 out of range"))
        );
        // The hostile extreme R3-L6 measured: u32::MAX, which used to overflow `+ 4`.
        let huge = sps_prefix(&|b: &mut Bits| b.ue_with_zeros(32, 0));
        assert_eq!(
            parse_sps(&huge),
            Err(HwDecError::Bitstream("SPS log2_max_poc_lsb_minus4 out of range"))
        );
        // …and 12 — the largest LEGAL value — is NOT what the new gate refuses: this fixture stops
        // right after the field, so it dies of the truncation instead. That is the discriminator
        // that says the gate bit on the RANGE and not on the fixture being short.
        let ok = sps_prefix(&|b: &mut Bits| b.ue(12));
        assert_eq!(parse_sps(&ok), Err(HwDecError::Bitstream("SPS ended mid-field")));
    }

    #[test]
    fn a_truncated_hvcc_is_an_error_not_a_panic() {
        assert!(parse_hvcc(&[]).is_err());
        assert!(parse_hvcc(&[1u8; 22]).is_err());
        let mut body = vec![0u8; 23];
        body[0] = 1;
        body[22] = 1; // one array…
        assert!(parse_hvcc(&body).is_err(), "…whose header is not there");
    }
}
