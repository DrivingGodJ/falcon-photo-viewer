//! v0.8.145 (E2) — **the deterministic NV12 → RGB8 kernel**: the colour stage of the
//! hardware-decode epic (July 2026).
//!
//! # Why this module is the whole fidelity story
//!
//! Stage 0 measured what the investigation had only sourced: HEVC decode is **bit-exact**. One real
//! 896×1024 tile through NVDEC and through libavcodec's software decoder came back byte-identical,
//! 0 of 1,376,256 bytes different, on both an iOS 26.3 file and an iOS 27.0 one. Every backend the
//! epic can ever adopt — NVDEC, D3D11VA, oneVPL, AMF, a software fallback — hands back *the same
//! NV12*. So there is exactly one place where two Falcon builds, two GPUs or two driver versions
//! could disagree about a HEIC's colour, and it is the arithmetic below.
//!
//! That is why this kernel is OURS and not the driver's: `cuvidMapVideoFrame`-style colour
//! conversion, D3D11 video processors and WIC each apply their own chroma filter and their own
//! rounding, none of them specified, none of them stable across vendors. Non-negotiable #2 of the
//! plan says one kernel of our own, shared by every backend. This module is the CPU half; the GPU
//! half is `falcon_gpu::Nv12Kernel`, and [`GOLDEN_PINS`] is the table both are held to.
//!
//! # INERT
//!
//! Nothing on the shipping decode path calls anything in this module. There is no environment flag,
//! because there is nothing to switch: the only callers are the tests, the golden-pin harness in
//! `falcon-gpu`, and the `yuv_kernel_probe` example. `FALCON_CLASSIC_HEIC` and
//! `FALCON_HW_HEIC_PARSE` (E1) are untouched and orthogonal.
//!
//! # What the real files declare (measured, 2026-08-04 — do not re-guess this)
//!
//! The SPS VUI of the PRIMARY image was parsed out of the real `hvcC` records of all six corpus
//! HEICs (five testkit + the fresh iOS 27 `IMG_3258`), and independently corroborated by `ffprobe`
//! on the extracted tile bitstreams. **All six agree, exactly:**
//!
//! | VUI field | value | meaning |
//! |---|---|---|
//! | `video_full_range_flag` | 1 | **FULL range** — Y 0..255, not 16..235 |
//! | `matrix_coeffs` | 6 | **BT.601** (SMPTE 170M) — *not* BT.709 |
//! | `colour_primaries` | 12 | SMPTE ST 432-1 = Display P3 (agrees with the 536-byte `prof` ICC) |
//! | `transfer_characteristics` | 1 | BT.709 |
//! | `chroma_loc_info_present_flag` | 0 | absent → H.265 spec default 0 = **LEFT siting** |
//!
//! Two of those are traps worth naming. A modern 48 MP phone camera declaring **BT.601** looks like
//! a typo and is not — Apple has shipped 601 in HEIC since day one, and assuming 709 tints every
//! photo. And **FULL range** means the limited-range `(Y-16)·255/219` expansion that most YUV code
//! reaches for first would crush blacks and clip highlights on every single one of these files.
//! Neither is guessed here: [`params_from_vui`] takes the parsed numbers and refuses anything it
//! does not model, so E3 cannot quietly default its way into either mistake. (L42: the answering
//! bytes are in the file — read them.)
//!
//! # The arithmetic, stated exactly
//!
//! Every step is integer. There is no float anywhere in this module or in its WGSL twin, because
//! the stage gate is byte-identity between them and float is exactly what makes that unprovable.
//!
//! **1. Coefficients.** Derived by [`YuvParams::coeffs`] from EXACT RATIONALS at compile time —
//! `Kr = 299/1000, Kb = 114/1000` (BT.601) or `Kr = 2126/10000, Kb = 722/10000` (BT.709) — never
//! transcribed from a table of decimals. Each lands as a **Q16 fixed-point i32** (multiplier
//! 65536), rounded half-away-from-zero by integer arithmetic. Limited range additionally scales
//! luma by `255/219` and chroma by `255/224`; full range scales by 1.
//!
//! **2. Chroma upsample.** Bilinear, at the declared siting, **exact — no rounding at all**. The
//! two chroma samples are carried at ×16 precision straight into the matrix rather than being
//! re-quantised to 8 bits first, so the upsample contributes zero error of its own.
//!
//! Sample positions, in luma-sample units with luma sample centres at integer coordinates
//! (H.265 Figure E-1):
//!
//! * [`ChromaSiting::Left`] (`chroma_sample_loc_type` 0 — the HEVC default, and what every corpus
//!   file gets): chroma sample `i` sits at luma `x = 2i`; chroma row `j` sits at luma `y = 2j + ½`.
//! * [`ChromaSiting::Center`] (type 1 — JPEG/JFIF/MPEG-1 interstitial): chroma sample `i` sits at
//!   luma `x = 2i + ½`; vertical is the same `y = 2j + ½`.
//!
//! Both sitings are therefore vertically interstitial and differ **only horizontally**. The
//! resulting weights are always quarters, so with `fx4, fy4 ∈ {0,1,2,3}`:
//!
//! ```text
//! C16 = (4-fy4)·[(4-fx4)·C00 + fx4·C01] + fy4·[(4-fx4)·C10 + fx4·C11]      (0 ..= 4080 = 16·255)
//! ```
//!
//! Sample indices are clamped to the chroma plane (replicate-edge), which is what makes odd
//! widths/heights and the tile borders well-defined.
//!
//! **3. Matrix + range, one expression per channel.** With `Y` the 8-bit luma code and `U16`,`V16`
//! the ×16 upsampled chroma:
//!
//! ```text
//! N   = ay·(Y − y_off)·16  +  c_u·(U16 − 2048)  +  c_v·(V16 − 2048)     // = value·2^20
//! out = clamp( (N + 2^19) >> 20 , 0, 255 )
//! ```
//!
//! `>>` is an **arithmetic** shift in both Rust and WGSL, so `(N + 2^19) >> 20` is
//! `floor((N + 2^19) / 2^20)` for negative `N` too — i.e. **round-half-up (ties toward +∞)**,
//! identically on both sides. The clamp is the only saturation in the kernel and it is where
//! limited-range codes below 16 / above 235 (and chroma outside 16..240) are legally absorbed;
//! full range passes 0 and 255 straight through.
//!
//! `|N|` is bounded by 576 M against `i32::MAX` = 2147 M — a 3.7× margin, re-derived from the live
//! coefficient table by `coefficient_magnitude_bound_leaves_headroom` so a future coefficient can
//! never silently overflow the i32 the WGSL twin is stuck with.
//!
//! **4. Output.** Packed RGB8, 24 bpp, stride `w*3` — the contract from the plan's non-negotiable
//! #4, byte-for-byte what `finish_source` already produces.
//!
//! # What is NOT here, on purpose
//!
//! No grid compositing, no crop, no `irot`, no scaling, no colour management. Those are E3's and
//! the existing CM chain's. This kernel turns one NV12 surface into one RGB8 buffer and nothing
//! else — which is what lets [`GOLDEN_PINS`] mean something.

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Parameters — explicit, never guessed
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The YCbCr→RGB matrix family. Read from the SPS VUI's `matrix_coeffs`, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    /// `matrix_coeffs` 5 (BT.470BG) or 6 (SMPTE 170M). Kr = 0.299, Kb = 0.114.
    /// **This is what every iPhone HEIC in the corpus declares.**
    Bt601,
    /// `matrix_coeffs` 1. Kr = 0.2126, Kb = 0.0722.
    Bt709,
}

/// Quantisation range. Read from the SPS VUI's `video_full_range_flag`, never assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvRange {
    /// `video_full_range_flag` = 1. Y spans 0..255, chroma 0..255 about 128.
    /// **This is what every iPhone HEIC in the corpus declares.**
    Full,
    /// `video_full_range_flag` = 0. Y spans 16..235, chroma 16..240 about 128; out-of-range codes
    /// are legal in the bitstream and are absorbed by the output clamp.
    Limited,
}

/// Horizontal chroma siting for 4:2:0. Both variants are vertically interstitial; they differ only
/// horizontally (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaSiting {
    /// `chroma_sample_loc_type` 0 — co-sited with the even luma column. The H.265 default when
    /// `chroma_loc_info_present_flag` is 0, **which is the case for every corpus file**.
    Left,
    /// `chroma_sample_loc_type` 1 — halfway between luma columns (JPEG/JFIF/MPEG-1).
    Center,
}

/// The full parameter set the kernel needs. Constructed from the container/VUI by
/// [`params_from_vui`]; there is no `Default`, deliberately — a caller must say what it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvParams {
    pub matrix: YuvMatrix,
    pub range: YuvRange,
    pub siting: ChromaSiting,
}

/// The Q16 fixed-point coefficients one `(matrix, range)` pair resolves to. Public because the GPU
/// twin uploads *these exact integers* as a uniform — the numbers are single-source, and only the
/// arithmetic is written twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvCoeffs {
    /// Luma gain, Q16. 65536 (×1) for full range, 76309 (×255/219) for limited.
    pub ay: i32,
    /// Luma offset subtracted before the gain: 0 (full) or 16 (limited).
    pub y_off: i32,
    /// R += `r_cr` · (V−128), Q16.
    pub r_cr: i32,
    /// G += `g_cb` · (U−128), Q16 (negative).
    pub g_cb: i32,
    /// G += `g_cr` · (V−128), Q16 (negative).
    pub g_cr: i32,
    /// B += `b_cb` · (U−128), Q16.
    pub b_cb: i32,
}

/// Q16 fixed point: `round_half_away_from_zero(num / den * 65536)`, in pure integer arithmetic so
/// the value is identical on every machine and available at compile time. `num * 65536` is bounded
/// by ~3.4e12 for the largest numerator in the table, well inside i64.
const fn q16(num: i64, den: i64) -> i32 {
    let n = num * 65536;
    let half = den / 2;
    let r = if n >= 0 { (n + half) / den } else { (n - half) / den };
    r as i32
}

impl YuvParams {
    /// Resolve the Q16 coefficient set. `const fn`, from exact rationals — see the module docs.
    ///
    /// The colour-difference coefficients are the textbook identities, kept as fractions so no
    /// decimal ever gets transcribed:
    ///
    /// ```text
    /// r_cr = 2(1−Kr)      b_cb = 2(1−Kb)
    /// g_cb = −2·Kb(1−Kb)/Kg      g_cr = −2·Kr(1−Kr)/Kg       Kg = 1 − Kr − Kb
    /// ```
    ///
    /// Limited range then scales luma by 255/219 and both chroma axes by 255/224 — the inverse of
    /// the standard `Y = 219·E'Y + 16`, `C = 224·E'PB + 128` quantisation.
    pub const fn coeffs(&self) -> YuvCoeffs {
        // (kr_num, kb_num, k_den) — exact.
        let (kr, kb, kd): (i64, i64, i64) = match self.matrix {
            YuvMatrix::Bt601 => (299, 114, 1000),
            YuvMatrix::Bt709 => (2126, 722, 10000),
        };
        let kg = kd - kr - kb; // over the same denominator kd
        // r_cr = 2(kd-kr)/kd ; b_cb = 2(kd-kb)/kd
        let r_cr_n = 2 * (kd - kr);
        let r_cr_d = kd;
        let b_cb_n = 2 * (kd - kb);
        let b_cb_d = kd;
        // g_cb = -2·kb·(kd-kb) / (kd·kg) ; g_cr = -2·kr·(kd-kr) / (kd·kg)
        let g_cb_n = -2 * kb * (kd - kb);
        let g_cb_d = kd * kg;
        let g_cr_n = -2 * kr * (kd - kr);
        let g_cr_d = kd * kg;
        match self.range {
            YuvRange::Full => YuvCoeffs {
                ay: q16(1, 1),
                y_off: 0,
                r_cr: q16(r_cr_n, r_cr_d),
                g_cb: q16(g_cb_n, g_cb_d),
                g_cr: q16(g_cr_n, g_cr_d),
                b_cb: q16(b_cb_n, b_cb_d),
            },
            YuvRange::Limited => YuvCoeffs {
                ay: q16(255, 219),
                y_off: 16,
                r_cr: q16(r_cr_n * 255, r_cr_d * 224),
                g_cb: q16(g_cb_n * 255, g_cb_d * 224),
                g_cr: q16(g_cr_n * 255, g_cr_d * 224),
                b_cb: q16(b_cb_n * 255, b_cb_d * 224),
            },
        }
    }
}

/// Everything the kernel can refuse. Every arm is a REFUSAL, never a silent default — the whole
/// point of the module is that colour parameters are read, not guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YuvKernelError {
    /// Zero width or height.
    EmptyFrame,
    /// A plane is shorter than the declared stride × rows demands.
    ShortPlane { plane: &'static str, need: usize, got: usize },
    /// A declared stride is narrower than the row it must hold.
    StrideTooSmall { plane: &'static str, need: usize, got: usize },
    /// `matrix_coeffs` names a matrix this kernel does not implement. NOT defaulted — a BT.2020 or
    /// YCgCo file must fall soft to the WIC path rather than be rendered with the wrong matrix.
    UnsupportedMatrix(u8),
    /// `chroma_sample_loc_type` names a siting this kernel does not implement (2..=5 are the
    /// vertically co-sited variants, which no corpus file uses and which would need a second
    /// vertical phase).
    UnsupportedSiting(u8),
    /// The frame is larger than the kernel's proven i32 arithmetic window.
    FrameTooLarge { w: u32, h: u32 },
}

impl std::fmt::Display for YuvKernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YuvKernelError::EmptyFrame => write!(f, "NV12 frame has zero width or height"),
            YuvKernelError::ShortPlane { plane, need, got } => {
                write!(f, "NV12 {plane} plane too short: need {need} bytes, got {got}")
            }
            YuvKernelError::StrideTooSmall { plane, need, got } => {
                write!(f, "NV12 {plane} stride {got} is narrower than the {need}-byte row")
            }
            YuvKernelError::UnsupportedMatrix(v) => {
                write!(f, "VUI matrix_coeffs {v} is not modelled by the Falcon YUV kernel")
            }
            YuvKernelError::UnsupportedSiting(v) => {
                write!(f, "VUI chroma_sample_loc_type {v} is not modelled by the Falcon YUV kernel")
            }
            YuvKernelError::FrameTooLarge { w, h } => {
                write!(f, "NV12 frame {w}x{h} exceeds the kernel's addressable window")
            }
        }
    }
}

impl std::error::Error for YuvKernelError {}

/// Turn the numbers a container/VUI actually carries into a [`YuvParams`], or refuse.
///
/// * `matrix_coeffs` — the VUI field. 1 → BT.709; 5 or 6 → BT.601. Everything else, **including 2
///   (`UNSPECIFIED`)**, is refused: "unspecified" is not a licence to pick one.
/// * `full_range` — `video_full_range_flag`.
/// * `chroma_loc` — `chroma_sample_loc_type_top_field` when
///   `chroma_loc_info_present_flag` is 1, or `None` when it is absent. `None` resolves to the H.265
///   default of 0 = [`ChromaSiting::Left`], which is the case for **every corpus file**.
pub fn params_from_vui(
    matrix_coeffs: u8,
    full_range: bool,
    chroma_loc: Option<u8>,
) -> Result<YuvParams, YuvKernelError> {
    let matrix = match matrix_coeffs {
        1 => YuvMatrix::Bt709,
        5 | 6 => YuvMatrix::Bt601,
        other => return Err(YuvKernelError::UnsupportedMatrix(other)),
    };
    let siting = match chroma_loc.unwrap_or(0) {
        0 => ChromaSiting::Left,
        1 => ChromaSiting::Center,
        other => return Err(YuvKernelError::UnsupportedSiting(other)),
    };
    let range = if full_range { YuvRange::Full } else { YuvRange::Limited };
    Ok(YuvParams { matrix, range, siting })
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The frame
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A borrowed NV12 surface: a full-resolution Y plane and a half-resolution **interleaved**
/// Cb,Cr plane (`u0 v0 u1 v1 …`), each with its own row stride.
///
/// Strides are explicit because hardware decoders hand back *pitched* surfaces — NVDEC's
/// `cuvidMapVideoFrame` and D3D11's `Map` both report a pitch that is nothing like the width — and
/// a kernel that assumed `stride == w` would force E3 into a pointless repack of every tile.
#[derive(Debug, Clone, Copy)]
pub struct Nv12Frame<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    /// Interleaved Cb,Cr at `ceil(w/2) × ceil(h/2)`; `uv_stride` counts BYTES, so a tight plane has
    /// `uv_stride == 2 * ceil(w/2)`.
    pub uv: &'a [u8],
    pub uv_stride: usize,
    pub w: u32,
    pub h: u32,
}

impl<'a> Nv12Frame<'a> {
    /// The tightly-packed case: `y_stride = w`, `uv_stride = 2·ceil(w/2)`.
    pub fn packed(y: &'a [u8], uv: &'a [u8], w: u32, h: u32) -> Nv12Frame<'a> {
        Nv12Frame { y, y_stride: w as usize, uv, uv_stride: 2 * w.div_ceil(2) as usize, w, h }
    }

    /// Chroma plane dimensions for this frame: `ceil(w/2) × ceil(h/2)`.
    pub fn chroma_dims(&self) -> (u32, u32) {
        (self.w.div_ceil(2), self.h.div_ceil(2))
    }

    /// Every way this frame is not one, as a NAMED error rather than an index panic. Returns the
    /// chroma dimensions, so a caller that validates does not then recompute them.
    ///
    /// v0.8.152 (R3-L1): `pub` because the GPU twin has to call the SAME function. It was private,
    /// so `falcon_gpu::Nv12Kernel::convert_with_coeffs` had only a `w == 0 || h == 0` check and then
    /// SLICED — meaning the pair whose byte-identity is the epic's entire colour argument answered
    /// `Err(ShortPlane)` on one side and panicked on the other for the same `Nv12Frame`.
    pub fn validate(&self) -> Result<(u32, u32), YuvKernelError> {
        if self.w == 0 || self.h == 0 {
            return Err(YuvKernelError::EmptyFrame);
        }
        // The i32 arithmetic window: indices are computed in i32, so keep both axes inside 2^30.
        if self.w > (1 << 15) || self.h > (1 << 15) {
            return Err(YuvKernelError::FrameTooLarge { w: self.w, h: self.h });
        }
        let (cw, ch) = self.chroma_dims();
        if self.y_stride < self.w as usize {
            return Err(YuvKernelError::StrideTooSmall {
                plane: "Y",
                need: self.w as usize,
                got: self.y_stride,
            });
        }
        if self.uv_stride < 2 * cw as usize {
            return Err(YuvKernelError::StrideTooSmall {
                plane: "UV",
                need: 2 * cw as usize,
                got: self.uv_stride,
            });
        }
        // Last row need only be `w` (resp. 2·cw) long — a trailing pitch is not required to exist.
        let y_need = self.y_stride * (self.h as usize - 1) + self.w as usize;
        if self.y.len() < y_need {
            return Err(YuvKernelError::ShortPlane { plane: "Y", need: y_need, got: self.y.len() });
        }
        let uv_need = self.uv_stride * (ch as usize - 1) + 2 * cw as usize;
        if self.uv.len() < uv_need {
            return Err(YuvKernelError::ShortPlane { plane: "UV", need: uv_need, got: self.uv.len() });
        }
        Ok((cw, ch))
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The kernel
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The rounding addend: half of the 2^20 scale the numerator carries. Named because the WGSL twin
/// hard-codes the same literal and the falsifier flips exactly this constant.
pub const ROUND_ADDEND: i32 = 1 << 19;
/// The numerator's fixed-point scale: Q16 coefficients × the ×16 chroma precision.
pub const NUM_SHIFT: u32 = 20;

/// Per-pixel horizontal chroma tap: `(i0, fx4)` — the left chroma index (which may be −1 and is
/// clamped by the caller) and the right-hand weight in quarters.
///
/// Left siting: chroma `i` is at luma `2i`, so the position in chroma units is `x/2` — even luma
/// columns land exactly on a chroma sample (`fx4 = 0`), odd ones split it evenly (`fx4 = 2`).
///
/// Centre siting: chroma `i` is at luma `2i + ½`, so the position is `x/2 − ¼`. Written in quarter
/// units and biased by one whole chroma sample (`q = 2x + 3`) so the shift is over a non-negative
/// value on both sides of the twin, then de-biased — `x = 0` correctly yields `i0 = −1`, which the
/// edge clamp turns into the replicate the geometry asks for.
#[inline]
fn h_tap(x: i32, siting: ChromaSiting) -> (i32, i32) {
    match siting {
        ChromaSiting::Left => (x >> 1, (x & 1) * 2),
        ChromaSiting::Center => {
            let q = 2 * x + 3;
            ((q >> 2) - 1, q & 3)
        }
    }
}

/// Per-pixel vertical chroma tap. Both sitings are vertically interstitial — chroma row `j` sits at
/// luma `2j + ½` — so this is the centre-siting formula unconditionally.
#[inline]
fn v_tap(y: i32) -> (i32, i32) {
    let q = 2 * y + 3;
    ((q >> 2) - 1, q & 3)
}

/// **The kernel.** NV12 in, packed RGB8 24 bpp (stride `w*3`) out.
///
/// Deterministic to the byte on any machine: integer-only, no float, no platform intrinsic, no
/// iteration-order dependence. The WGSL twin `falcon_gpu::Nv12Kernel` reproduces it exactly, and
/// `falcon-gpu/tests/yuv_kernel_twin.rs` is the gate that says so.
pub fn nv12_to_rgb8(f: &Nv12Frame, p: YuvParams) -> Result<Vec<u8>, YuvKernelError> {
    let (cw, ch) = f.validate()?;
    let c = p.coeffs();
    let (w, h) = (f.w as i32, f.h as i32);
    let (cwi, chi) = (cw as i32, ch as i32);
    let mut out = vec![0u8; (f.w as usize) * (f.h as usize) * 3];

    for y in 0..h {
        let (j0, fy4) = v_tap(y);
        let j0c = j0.clamp(0, chi - 1) as usize;
        let j1c = (j0 + 1).clamp(0, chi - 1) as usize;
        let uv_row0 = j0c * f.uv_stride;
        let uv_row1 = j1c * f.uv_stride;
        let y_row = (y as usize) * f.y_stride;
        let out_row = (y as usize) * (f.w as usize) * 3;
        for x in 0..w {
            let (i0, fx4) = h_tap(x, p.siting);
            let i0c = i0.clamp(0, cwi - 1) as usize;
            let i1c = (i0 + 1).clamp(0, cwi - 1) as usize;

            // Four (Cb,Cr) taps. NV12 interleaves them, so chroma sample i lives at byte 2i.
            let (u00, v00) = (f.uv[uv_row0 + 2 * i0c] as i32, f.uv[uv_row0 + 2 * i0c + 1] as i32);
            let (u01, v01) = (f.uv[uv_row0 + 2 * i1c] as i32, f.uv[uv_row0 + 2 * i1c + 1] as i32);
            let (u10, v10) = (f.uv[uv_row1 + 2 * i0c] as i32, f.uv[uv_row1 + 2 * i0c + 1] as i32);
            let (u11, v11) = (f.uv[uv_row1 + 2 * i1c] as i32, f.uv[uv_row1 + 2 * i1c + 1] as i32);

            // Exact bilinear at ×16 precision — no intermediate rounding (see the module docs).
            let gx = 4 - fx4;
            let gy = 4 - fy4;
            let u16v = gy * (gx * u00 + fx4 * u01) + fy4 * (gx * u10 + fx4 * u11);
            let v16v = gy * (gx * v00 + fx4 * v01) + fy4 * (gx * v10 + fx4 * v11);
            let du = u16v - 2048; // 2048 == 128 × 16
            let dv = v16v - 2048;

            let yv = f.y[y_row + x as usize] as i32;
            let yt = c.ay * (yv - c.y_off) * 16;
            let r = (yt + c.r_cr * dv + ROUND_ADDEND) >> NUM_SHIFT;
            let g = (yt + c.g_cb * du + c.g_cr * dv + ROUND_ADDEND) >> NUM_SHIFT;
            let b = (yt + c.b_cb * du + ROUND_ADDEND) >> NUM_SHIFT;

            let o = out_row + (x as usize) * 3;
            out[o] = r.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = b.clamp(0, 255) as u8;
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The digest — the new path's byte-pin discipline
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// SHA-256 of `data`, lowercase hex.
///
/// Written out here rather than pulled in as a dependency: falcon-decode's whole dependency story
/// is "pure Rust, no cmake/nasm", and 60 lines of FIPS 180-4 is cheaper than another crate. It is
/// the REAL SHA-256, not a house hash, and that is the point — every digest in [`GOLDEN_PINS`] can
/// be re-derived by anyone with `sha256sum` or `Get-FileHash` over a dumped RGB buffer, with no
/// Falcon code in the loop. `sha256_matches_an_external_oracle` pins it against the published
/// NIST test vectors.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let block = |b: &[u8], h: &mut [u32; 8]| {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b_, mut c, mut d, mut e, mut f_, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let chv = (e & f_) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(chv)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b_) ^ (a & c) ^ (b_ & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f_;
            f_ = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b_;
            b_ = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b_);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f_);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    };
    let full = data.len() / 64;
    for i in 0..full {
        block(&data[i * 64..i * 64 + 64], &mut h);
    }
    // Tail + FIPS 180-4 padding: 0x80, zeros, then the 64-bit big-endian bit length.
    let rest = &data[full * 64..];
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let bits = (data.len() as u64).wrapping_mul(8);
    let tail_len = if rest.len() < 56 { 64 } else { 128 };
    tail[tail_len - 8..tail_len].copy_from_slice(&bits.to_be_bytes());
    block(&tail[..64], &mut h);
    if tail_len == 128 {
        block(&tail[64..128], &mut h);
    }
    let mut s = String::with_capacity(64);
    for v in h {
        s.push_str(&format!("{v:08x}"));
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Synthetic inputs — deterministic, integer, reproducible in any language
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The synthetic NV12 patterns the golden table is built on. Each is a closed-form integer
/// function of the sample coordinate, so a reader can regenerate the exact input in five lines of
/// any language and re-derive the pinned digest without running Falcon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvSynthPattern {
    /// **The lattice.** `Y = (x + 4y) & 255`; chroma `U = i & 255`, `V = j & 255`. At 512×512 the
    /// chroma plane is exactly 256×256, so **every one of the 65 536 (U,V) pairs appears**, each of
    /// the 256 luma codes appears throughout, and all four chroma corners (0,0)/(255,0)/(0,255)/
    /// (255,255) are present. This is the row that says the matrix is right everywhere, not just
    /// near grey.
    Cube,
    /// **The extremes.** Every sample drawn from an 18-code set clustered on the range boundaries —
    /// 0, 1, 15, 16, 17, 234, 235, 236, 239, 240, 241, 254, 255 and four mid codes. This is where
    /// the limited-range clamp is exercised: sub-16 and over-235 luma, sub-16 and over-240 chroma.
    Extremes,
    /// **The upsample stress.** A 1-pixel luma checkerboard over a 1-sample chroma checkerboard —
    /// the highest spatial frequency 4:2:0 can carry, so every bilinear phase runs at full
    /// amplitude and a siting error is a visible, digest-breaking shift rather than a rounding
    /// nudge.
    Edges,
    /// **The ramp.** `Y = (7x + 13y) & 255`, `U = 11i & 255`, `V = 17j & 255`. Coprime strides, so
    /// small odd-dimensioned frames still carry varied data — used for the edge-clamp rows.
    Ramp,
}

/// Build a synthetic NV12 pair `(y_plane, uv_plane)`, tightly packed.
pub fn yuv_synth_nv12(p: YuvSynthPattern, w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    const CRIT: [u8; 18] =
        [0, 1, 15, 16, 17, 64, 96, 127, 128, 129, 191, 234, 235, 236, 239, 240, 241, 255];
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; (w * h) as usize];
    let mut uv = vec![0u8; (2 * cw * ch) as usize];
    for row in 0..h {
        for col in 0..w {
            let v = match p {
                YuvSynthPattern::Cube => ((col + 4 * row) & 0xFF) as u8,
                YuvSynthPattern::Extremes => CRIT[((col + row) % 18) as usize],
                YuvSynthPattern::Edges => {
                    if (col & 1) ^ (row & 1) == 1 {
                        0
                    } else {
                        255
                    }
                }
                YuvSynthPattern::Ramp => ((7 * col + 13 * row) & 0xFF) as u8,
            };
            y[(row * w + col) as usize] = v;
        }
    }
    for j in 0..ch {
        for i in 0..cw {
            let (u, v) = match p {
                YuvSynthPattern::Cube => ((i & 0xFF) as u8, (j & 0xFF) as u8),
                YuvSynthPattern::Extremes => {
                    (CRIT[((i * 3 + j) % 18) as usize], CRIT[((i + j * 5) % 18) as usize])
                }
                YuvSynthPattern::Edges => (
                    if i & 1 == 0 { 0 } else { 255 },
                    if j & 1 == 0 { 255 } else { 0 },
                ),
                YuvSynthPattern::Ramp => (((11 * i) & 0xFF) as u8, ((17 * j) & 0xFF) as u8),
            };
            uv[(2 * (j * cw + i)) as usize] = u;
            uv[(2 * (j * cw + i) + 1) as usize] = v;
        }
    }
    (y, uv)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The golden table — ONE table, two consumers
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Where a golden case's NV12 comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvGoldenSrc {
    /// Generated by [`yuv_synth_nv12`] — costs no repository bytes.
    Synth(YuvSynthPattern),
    /// A real decoded HEVC tile, by fixture file name. The consumer supplies the bytes (see
    /// [`yuv_golden_planes`]); the kernel crate deliberately does NOT `include_bytes!` them, so no
    /// test data is linked into the shipping binary.
    Fixture(&'static str),
}

/// One pinned row: an input, a parameter set, and the SHA-256 the packed RGB8 output must have.
#[derive(Debug, Clone, Copy)]
pub struct YuvGoldenCase {
    pub name: &'static str,
    pub src: YuvGoldenSrc,
    pub w: u32,
    pub h: u32,
    pub params: YuvParams,
    /// SHA-256 (lowercase hex) of the `w*h*3` packed RGB8 output.
    pub digest: &'static str,
}

// Eight arguments, on purpose: spelling the parameter triple out per row is what makes the table
// below readable as a matrix of the parameter space. Collapsing them into a `YuvParams` literal per
// row costs three lines of ceremony each and hides exactly the axis a reader is scanning for.
#[allow(clippy::too_many_arguments)]
const fn gc(
    name: &'static str,
    src: YuvGoldenSrc,
    w: u32,
    h: u32,
    matrix: YuvMatrix,
    range: YuvRange,
    siting: ChromaSiting,
    digest: &'static str,
) -> YuvGoldenCase {
    YuvGoldenCase { name, src, w, h, params: YuvParams { matrix, range, siting }, digest }
}

/// The two real-tile fixtures, by name. Each is a **256×256 NV12 excerpt of a real decoded HEVC
/// tile** — the highest-entropy 256×256 block of the highest-payload tile of the file, at an even
/// origin so the 4:2:0 chroma phase is preserved exactly.
///
/// Real rather than synthetic because synthetic data cannot reproduce a camera's actual chroma
/// statistics; an excerpt rather than the whole 896×1024 tile because the whole tile is 1.31 MB and
/// the excerpt already carries all 256 luma codes and 140-odd distinct chroma codes. The full-tile
/// digests are measured by `examples/yuv_kernel_probe.rs` and recorded in the v0.8.145 commit
/// message; these are the rows that run on every `cargo test`.
pub const REAL_TILE_FIXTURES: &[&str] = &[
    "heic_IMG_1826_t22_256x256.nv12",
    "heic_IMG_3258_t40_256x256.nv12",
];

/// **The digest set.** The byte-pin discipline for the hardware path is born here — a set of its
/// own, entirely parallel to the JPEG byte-pin machinery, which this round does not touch.
///
/// Both consumers iterate this ONE table:
///
/// * `falcon-decode/tests/yuv_kernel_pins.rs` runs the CPU reference and asserts the digest;
/// * `falcon-gpu/tests/yuv_kernel_twin.rs` runs the WGSL twin, asserts the digest, **and** asserts
///   the output bytes equal the CPU reference's byte for byte.
///
/// So a drift between the twins and a drift from the pin are two separate failures, and neither
/// can be papered over by editing one side.
pub const GOLDEN_PINS: &[YuvGoldenCase] = &[
    // ── The lattice: full (U,V) product × all luma codes, all four matrix/range combinations ──
    gc(
        "cube-512x512-601-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Cube),
        512,
        512,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "d0e3f9e20d36c2e0e6efdc53b9959ff7cdf57667075cccd45fa0b5f65bd89631",
    ),
    gc(
        "cube-512x512-601-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Cube),
        512,
        512,
        YuvMatrix::Bt601,
        YuvRange::Limited,
        ChromaSiting::Left,
        "d22dcc7a0367881c8415c72616a7b767c16002271b30d80123a11172f2e525f3",
    ),
    gc(
        "cube-512x512-709-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Cube),
        512,
        512,
        YuvMatrix::Bt709,
        YuvRange::Full,
        ChromaSiting::Left,
        "2677bb375f9ae9b740c9568add1b6a28646d1ed41e139528bc5d580a00fc3b67",
    ),
    gc(
        "cube-512x512-709-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Cube),
        512,
        512,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Left,
        "67044d3537e976a41e8cf23be5251c79f8e68724c1a01bcab2aa75d68d502a7e",
    ),
    // The siting axis on the same lattice — Left vs Center must NOT agree.
    gc(
        "cube-512x512-601-full-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Cube),
        512,
        512,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Center,
        "405c186755d0b34e31556b962fed9abf892ba735e849543122906635878d596b",
    ),
    // ── The extremes: where the limited-range clamp lives ──
    gc(
        "extremes-36x18-601-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Extremes),
        36,
        18,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "fe65aff850a5a5542d8c774fd06f0b204e8a1bfeb7bc60971a732be3887506c1",
    ),
    gc(
        "extremes-36x18-601-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Extremes),
        36,
        18,
        YuvMatrix::Bt601,
        YuvRange::Limited,
        ChromaSiting::Left,
        "4fe2d9c4ec13c5244729c343fc0e9d5492d9fe5f3bfb490b07707885818d051b",
    ),
    gc(
        "extremes-36x18-709-full-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Extremes),
        36,
        18,
        YuvMatrix::Bt709,
        YuvRange::Full,
        ChromaSiting::Center,
        "dc9d145f74b127eaef52186a34fa7142e0f56ed104e582d1cebc1e4d141b42f5",
    ),
    gc(
        "extremes-36x18-709-limited-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Extremes),
        36,
        18,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Center,
        "8cff84505df5e0bf03ed88b855915592fbf988f257e9bd021b5861c7c0b4da64",
    ),
    // ── The upsample stress: every bilinear phase at full amplitude ──
    gc(
        "edges-64x64-601-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Edges),
        64,
        64,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "13ce149cb318f87efb678a70baf9822ee82d770b45215d72c874672db4af0423",
    ),
    gc(
        "edges-64x64-601-full-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Edges),
        64,
        64,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Center,
        "0480e218009749c2b33c0aed8a6e49446b7df1e6038e726391d0f9684c463ebc",
    ),
    gc(
        "edges-64x64-709-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Edges),
        64,
        64,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Left,
        "79f7f874daa533d7dc2ca5a3cf453b6f0ece91f5897ee661c9ed4680b9de51a6",
    ),
    // ── Odd dimensions: the right/bottom chroma clamp, and the 1-sample degenerate cases ──
    gc(
        "ramp-7x5-601-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        7,
        5,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "58791e6fb2a2fc81377f7843163f484d98549d985432298a6ef036fa1da1606d",
    ),
    gc(
        "ramp-7x5-601-full-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        7,
        5,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Center,
        "1ff06f722996467697927afa23f2d71605319fb189132f0b1784a2de823891b3",
    ),
    gc(
        "ramp-15x4-709-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        15,
        4,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Left,
        "63645e22ffacd87cf85a9722fd4f0d83ffc7f76769bf2c4e756408da98729734",
    ),
    gc(
        "ramp-4x15-709-limited-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        4,
        15,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Left,
        "6af0bf271ba3b4da12b0a1a429ae0d9d1759a67c88aa8e6fad8f555d99eac937",
    ),
    gc(
        "ramp-1x1-601-full-left",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        1,
        1,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "b744af5a13a2e6aa44e05583a887efe772e46965228805b5daa98cb85b5c4b2b",
    ),
    gc(
        "ramp-9x1-601-limited-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        9,
        1,
        YuvMatrix::Bt601,
        YuvRange::Limited,
        ChromaSiting::Center,
        "62010c2c34db8b4d3de2a45260fefbb646821c8a66e01dfed3db09aa5ecc8d1d",
    ),
    gc(
        "ramp-1x9-601-limited-center",
        YuvGoldenSrc::Synth(YuvSynthPattern::Ramp),
        1,
        9,
        YuvMatrix::Bt601,
        YuvRange::Limited,
        ChromaSiting::Center,
        "fbd35b4de39cce293cf85f2a20820039883a3e6147d3273621508fe5da8fd0c6",
    ),
    // ── Real decoded HEVC tiles. The FIRST row of each pair is the file's OWN declared
    //    parameters (BT.601 / full / left — see the module docs); the second proves the
    //    parameterisation reaches real data rather than only synthetic lattices.
    gc(
        "real-IMG_1826-t22-601-full-left",
        YuvGoldenSrc::Fixture("heic_IMG_1826_t22_256x256.nv12"),
        256,
        256,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "8b1845e64982af2077252354077350bf1b6ab4ac92831702830cc82d72f8b090",
    ),
    gc(
        "real-IMG_1826-t22-709-limited-center",
        YuvGoldenSrc::Fixture("heic_IMG_1826_t22_256x256.nv12"),
        256,
        256,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Center,
        "2b55429f34b6e6bd3a687be24fadc6b8e711039973a46885fc0d565d50579cf6",
    ),
    gc(
        "real-IMG_3258-t40-601-full-left",
        YuvGoldenSrc::Fixture("heic_IMG_3258_t40_256x256.nv12"),
        256,
        256,
        YuvMatrix::Bt601,
        YuvRange::Full,
        ChromaSiting::Left,
        "c761fba1d9d528edcdec7b2899528a2a14c564ecba66364ea518dfffef7c0cc0",
    ),
    gc(
        "real-IMG_3258-t40-709-limited-center",
        YuvGoldenSrc::Fixture("heic_IMG_3258_t40_256x256.nv12"),
        256,
        256,
        YuvMatrix::Bt709,
        YuvRange::Limited,
        ChromaSiting::Center,
        "d4edc6dbd1956cb776464f84def0c74adf00b5b1dbeea0e163384a215e50be64",
    ),
];

/// Materialise a golden case's NV12 planes.
///
/// `fixtures` maps a [`YuvGoldenSrc::Fixture`] name to its raw bytes — each test crate supplies its
/// own `include_bytes!` of the shared `crates/testdata/` file, so the digests stay single-source
/// here while no test data is linked into the library.
///
/// Panics on an unknown fixture name: a golden row whose data cannot be found is a broken pin, not
/// a skippable one.
pub fn yuv_golden_planes(c: &YuvGoldenCase, fixtures: &[(&str, &[u8])]) -> (Vec<u8>, Vec<u8>) {
    match c.src {
        YuvGoldenSrc::Synth(p) => yuv_synth_nv12(p, c.w, c.h),
        YuvGoldenSrc::Fixture(name) => {
            let bytes = fixtures
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("golden case {} wants missing fixture {name}", c.name))
                .1;
            let (cw, ch) = (c.w.div_ceil(2), c.h.div_ceil(2));
            let ylen = (c.w * c.h) as usize;
            let uvlen = (2 * cw * ch) as usize;
            assert_eq!(
                bytes.len(),
                ylen + uvlen,
                "fixture {name} is {} bytes; {}x{} NV12 needs {}",
                bytes.len(),
                c.w,
                c.h,
                ylen + uvlen
            );
            (bytes[..ylen].to_vec(), bytes[ylen..].to_vec())
        }
    }
}

/// Run one golden case through the CPU reference. Shared so the two harnesses build the input
/// identically and only the *converter* differs between them.
pub fn yuv_golden_cpu_rgb(c: &YuvGoldenCase, fixtures: &[(&str, &[u8])]) -> Vec<u8> {
    let (y, uv) = yuv_golden_planes(c, fixtures);
    let f = Nv12Frame::packed(&y, &uv, c.w, c.h);
    nv12_to_rgb8(&f, c.params).expect("golden case must convert")
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Unit tests — the arithmetic's own falsifiers
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn p(m: YuvMatrix, r: YuvRange, s: ChromaSiting) -> YuvParams {
        YuvParams { matrix: m, range: r, siting: s }
    }

    /// The Q16 table, spelled out. Derived from exact rationals by `const fn`, so this test is the
    /// only place a human-readable number appears — if the derivation ever changes, this reddens
    /// with the old and new values side by side instead of the change sliding through as a digest
    /// churn nobody can explain.
    ///
    /// FALSIFIER: change any `Kr`/`Kb` rational in `coeffs` and one of these rows moves.
    #[test]
    fn q16_coefficients_are_the_published_ones() {
        // Full range = the textbook float coefficients × 65536, rounded half-away-from-zero.
        let c = p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Left).coeffs();
        assert_eq!(
            (c.ay, c.y_off, c.r_cr, c.g_cb, c.g_cr, c.b_cb),
            (65536, 0, 91881, -22553, -46802, 116130),
            "BT.601 full: 1.402 / -0.344136 / -0.714136 / 1.772"
        );
        let c = p(YuvMatrix::Bt709, YuvRange::Full, ChromaSiting::Left).coeffs();
        assert_eq!(
            (c.ay, c.y_off, c.r_cr, c.g_cb, c.g_cr, c.b_cb),
            (65536, 0, 103206, -12276, -30679, 121609),
            "BT.709 full: 1.5748 / -0.187324 / -0.468124 / 1.8556"
        );
        // Limited range = the same, × 255/224 on chroma, with the 255/219 luma gain.
        let c = p(YuvMatrix::Bt601, YuvRange::Limited, ChromaSiting::Left).coeffs();
        assert_eq!(
            (c.ay, c.y_off, c.r_cr, c.g_cb, c.g_cr, c.b_cb),
            (76309, 16, 104597, -25675, -53279, 132201),
            "BT.601 limited: 1.164384 / 1.596027 / -0.391762 / -0.812968 / 2.017232"
        );
        let c = p(YuvMatrix::Bt709, YuvRange::Limited, ChromaSiting::Left).coeffs();
        assert_eq!(
            (c.ay, c.y_off, c.r_cr, c.g_cb, c.g_cr, c.b_cb),
            (76309, 16, 117489, -13975, -34925, 138438),
            "BT.709 limited: 1.164384 / 1.792741 / -0.213249 / -0.532909 / 2.112402"
        );
    }

    /// The Q16 integers must agree with the float identities they stand for, to within the half-LSB
    /// the rounding allows. This is what makes the fixed-point choice a *representation* decision
    /// and not a silent change of colour.
    #[test]
    fn q16_coefficients_track_their_float_identities() {
        for (m, kr, kb) in [(YuvMatrix::Bt601, 0.299f64, 0.114f64), (YuvMatrix::Bt709, 0.2126, 0.0722)] {
            let kg = 1.0 - kr - kb;
            for (r, ys, cs) in
                [(YuvRange::Full, 1.0f64, 1.0f64), (YuvRange::Limited, 255.0 / 219.0, 255.0 / 224.0)]
            {
                let c = p(m, r, ChromaSiting::Left).coeffs();
                let want = [
                    ys,
                    2.0 * (1.0 - kr) * cs,
                    -2.0 * kb * (1.0 - kb) / kg * cs,
                    -2.0 * kr * (1.0 - kr) / kg * cs,
                    2.0 * (1.0 - kb) * cs,
                ];
                let got = [c.ay, c.r_cr, c.g_cb, c.g_cr, c.b_cb];
                for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
                    let err = (g as f64 - w * 65536.0).abs();
                    assert!(err <= 0.5, "coeff {i} for {m:?}/{r:?}: Q16 {g} vs {} (err {err})", w * 65536.0);
                }
            }
        }
    }

    /// The i32 window the WGSL twin is stuck with, re-derived from the LIVE coefficient table
    /// rather than asserted once and forgotten.
    ///
    /// FALSIFIER: raise any coefficient past the headroom and this reddens before a wrapped
    /// multiply can turn into a wrong pixel that no digest would explain.
    #[test]
    fn coefficient_magnitude_bound_leaves_headroom() {
        let mut worst = 0i64;
        for m in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            for r in [YuvRange::Full, YuvRange::Limited] {
                let c = p(m, r, ChromaSiting::Left).coeffs();
                // |Y - y_off| is at most 255 (full) or 239 (limited); chroma delta at most 2048.
                let ymax = (255 - c.y_off).max(c.y_off) as i64;
                let yterm = c.ay as i64 * ymax * 16;
                for chroma in [
                    c.r_cr.unsigned_abs() as i64,
                    c.b_cb.unsigned_abs() as i64,
                    c.g_cb.unsigned_abs() as i64 + c.g_cr.unsigned_abs() as i64,
                ] {
                    worst = worst.max(yterm + chroma * 2048 + ROUND_ADDEND as i64);
                }
            }
        }
        assert!(
            worst < i32::MAX as i64,
            "kernel numerator can reach {worst}, which does not fit i32 — the WGSL twin would wrap"
        );
        // Not merely "fits": keep a real margin, so a future coefficient tweak has room.
        assert!(worst * 2 < i32::MAX as i64, "less than 2x headroom left ({worst})");
        assert_eq!(worst, 575_850_928, "the proven bound, pinned so a change is visible");
    }

    /// Neutral chroma (128,128) must reproduce the luma ramp exactly — full range passes it
    /// through untouched, limited range expands 16..235 onto 0..255 with both ends landing dead on.
    ///
    /// FALSIFIER: drop the `>> NUM_SHIFT` rounding addend and the full-range identity breaks at
    /// every code (each becomes one low); use 219 instead of 255/219 and the endpoints miss.
    #[test]
    fn neutral_chroma_reproduces_the_luma_ramp() {
        for m in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            // Full range: Y in == Y out, all 256 codes, no exceptions.
            let y: Vec<u8> = (0..=255u8).collect();
            let uv = vec![128u8; 2 * 128];
            let f = Nv12Frame::packed(&y, &uv, 256, 1);
            let rgb = nv12_to_rgb8(&f, p(m, YuvRange::Full, ChromaSiting::Left)).unwrap();
            for code in 0..256usize {
                assert_eq!(
                    (rgb[code * 3], rgb[code * 3 + 1], rgb[code * 3 + 2]),
                    (code as u8, code as u8, code as u8),
                    "{m:?} full range must be an identity on neutral grey at code {code}"
                );
            }
            // Limited range: 16 -> 0, 235 -> 255, monotone in between, clamped outside.
            let rgb = nv12_to_rgb8(&f, p(m, YuvRange::Limited, ChromaSiting::Left)).unwrap();
            assert_eq!(rgb[16 * 3], 0, "{m:?} limited: code 16 is black");
            assert_eq!(rgb[235 * 3], 255, "{m:?} limited: code 235 is white");
            assert_eq!(rgb[15 * 3], 0, "{m:?} limited: code 15 clamps to black, it does not wrap");
            assert_eq!(rgb[0], 0, "{m:?} limited: code 0 clamps to black");
            assert_eq!(rgb[236 * 3], 255, "{m:?} limited: code 236 clamps to white");
            assert_eq!(rgb[255 * 3], 255, "{m:?} limited: code 255 clamps to white");
            for code in 16..235usize {
                assert!(
                    rgb[(code + 1) * 3] >= rgb[code * 3],
                    "{m:?} limited must be monotone across {code}"
                );
            }
        }
    }

    /// **The limited-range clamp, proven both ways** (the charter's third falsifier).
    ///
    /// A synthetic frame carrying sub-16 and over-235 luma plus sub-16/over-240 chroma:
    /// LIMITED must clamp the out-of-range codes, and FULL must pass the very same codes through
    /// as legal values. If the clamp were missing the limited numbers would wrap; if the range
    /// switch were ignored the two would agree.
    #[test]
    fn limited_range_clamps_what_full_range_passes_through() {
        // Neutral chroma isolates the luma clamp; the chroma clamp gets its own frame below.
        let y = [0u8, 8, 15, 16, 128, 235, 236, 250, 255];
        let uv = vec![128u8; 2 * 5];
        let f = Nv12Frame::packed(&y, &uv, 9, 1);
        let lim = nv12_to_rgb8(&f, p(YuvMatrix::Bt601, YuvRange::Limited, ChromaSiting::Left)).unwrap();
        let full = nv12_to_rgb8(&f, p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Left)).unwrap();
        // LIMITED: everything at or under 16 is black, everything at or over 235 is white.
        assert_eq!([lim[0], lim[3], lim[6], lim[9]], [0, 0, 0, 0], "sub-16 luma clamps to 0");
        assert_eq!([lim[15], lim[18], lim[21], lim[24]], [255, 255, 255, 255], "235+ clamps to 255");
        // FULL: the identical codes are ordinary values — 0 and 255 are the ends, not overflow.
        assert_eq!([full[0], full[3], full[6], full[9]], [0, 8, 15, 16], "full range passes them");
        assert_eq!([full[15], full[18], full[21], full[24]], [235, 236, 250, 255], "…both ends");
        // And the two must actually differ, or the range parameter is doing nothing.
        assert_ne!(lim, full, "LIMITED and FULL must not produce the same bytes");

        // Chroma out of the 16..240 window, at mid luma: limited must saturate a channel, and
        // nothing may wrap (a wrap shows up as a LOW value where a HIGH one is demanded).
        let y2 = vec![128u8; 4];
        let uv2 = [0u8, 255, 255, 0]; // (U,V) = (0,255) and (255,0) — opposite corners
        let f2 = Nv12Frame::packed(&y2, &uv2, 4, 1);
        let lim2 = nv12_to_rgb8(&f2, p(YuvMatrix::Bt601, YuvRange::Limited, ChromaSiting::Left)).unwrap();
        assert_eq!(lim2[0], 255, "V=255 at limited range saturates R, not wraps");
        assert_eq!(lim2[2], 0, "U=0 at limited range floors B, not wraps");
        assert_eq!(lim2[9], 0, "V=0 floors R");
        assert_eq!(lim2[11], 255, "U=255 saturates B");
    }

    /// The two sitings must place chroma differently — and specifically, LEFT must be an exact
    /// fetch on even columns while CENTRE never is.
    ///
    /// FALSIFIER: make `h_tap` return the same taps for both and this reddens; it is the test that
    /// stops the kernel quietly shipping JFIF siting for files that declare MPEG-2 siting.
    #[test]
    fn left_and_centre_siting_place_chroma_differently() {
        // A 1-sample chroma step: U jumps 0 -> 255 between chroma columns 0 and 1.
        let y = vec![128u8; 8];
        let uv = [0u8, 128, 255, 128, 255, 128, 255, 128];
        let f = Nv12Frame::packed(&y, &uv, 8, 1);
        let left = nv12_to_rgb8(&f, p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Left)).unwrap();
        let ctr = nv12_to_rgb8(&f, p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Center)).unwrap();
        assert_ne!(left, ctr, "the siting parameter must change the pixels");
        // LEFT: luma column 0 sits exactly on chroma sample 0 (U = 0) — an exact fetch, so B is
        // the full negative excursion. Column 2 sits exactly on chroma sample 1 (U = 255).
        let b = |v: &[u8], x: usize| v[x * 3 + 2];
        assert_eq!(b(&left, 0), 0, "LEFT column 0 is an exact fetch of U=0");
        assert_eq!(b(&left, 2), 255, "LEFT column 2 is an exact fetch of U=255");
        // Column 1 is the even 50/50 blend of the two — which is 127.5, i.e. HALF AN LSB BELOW the
        // neutral 128, so B lands at 127 and not at 128. That off-by-a-half is exactly the kind of
        // detail the ×16 chroma precision preserves and an 8-bit intermediate would have thrown
        // away, so it is worth asserting rather than rounding away in the test too.
        assert_eq!(b(&left, 1), 127, "LEFT column 1 is the half-and-half blend of 0 and 255");
        // CENTRE: no luma column ever lands on a chroma sample, so the extremes never appear in
        // the interior — the tell-tale of interstitial siting.
        assert!(b(&ctr, 1) > 0 && b(&ctr, 1) < 255, "CENTRE column 1 is interpolated");
        assert!(b(&ctr, 2) > 0 && b(&ctr, 2) < 255, "CENTRE column 2 is interpolated");
    }

    /// Chroma taps replicate at the plane edges, so odd dimensions and 1×1 frames are defined
    /// rather than out-of-bounds. (E1 reports 896×1024 tiles today, but a cropped ROI decode or a
    /// non-Apple file can hand the kernel anything.)
    #[test]
    fn odd_dimensions_and_degenerate_frames_are_defined() {
        for (w, h) in [(1u32, 1u32), (1, 9), (9, 1), (7, 5), (3, 3), (2, 1), (1, 2)] {
            let (y, uv) = yuv_synth_nv12(YuvSynthPattern::Ramp, w, h);
            let f = Nv12Frame::packed(&y, &uv, w, h);
            for s in [ChromaSiting::Left, ChromaSiting::Center] {
                let rgb = nv12_to_rgb8(&f, p(YuvMatrix::Bt601, YuvRange::Full, s)).expect("must convert");
                assert_eq!(rgb.len(), (w * h * 3) as usize, "{w}x{h} output is stride w*3");
            }
        }
    }

    /// A pitched surface (the shape every hardware decoder actually hands back) must produce
    /// byte-identical output to the tightly-packed same image. If it did not, E3 would be forced
    /// into a repack of every tile for no reason.
    #[test]
    fn a_pitched_surface_matches_the_packed_one() {
        let (w, h) = (37u32, 21u32);
        let (y, uv) = yuv_synth_nv12(YuvSynthPattern::Ramp, w, h);
        let packed = nv12_to_rgb8(
            &Nv12Frame::packed(&y, &uv, w, h),
            p(YuvMatrix::Bt709, YuvRange::Limited, ChromaSiting::Left),
        )
        .unwrap();
        // Re-lay the same planes at a 256-byte pitch, filling the pad with a value that would be
        // visible if it were ever read.
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (ys, uvs) = (256usize, 256usize);
        let mut yp = vec![0xA5u8; ys * h as usize];
        let mut uvp = vec![0x5Au8; uvs * ch as usize];
        for r in 0..h as usize {
            yp[r * ys..r * ys + w as usize].copy_from_slice(&y[r * w as usize..(r + 1) * w as usize]);
        }
        for r in 0..ch as usize {
            let n = 2 * cw as usize;
            uvp[r * uvs..r * uvs + n].copy_from_slice(&uv[r * n..(r + 1) * n]);
        }
        let pitched = nv12_to_rgb8(
            &Nv12Frame { y: &yp, y_stride: ys, uv: &uvp, uv_stride: uvs, w, h },
            p(YuvMatrix::Bt709, YuvRange::Limited, ChromaSiting::Left),
        )
        .unwrap();
        assert_eq!(packed, pitched, "a pitched NV12 surface must convert identically");
    }

    /// Every refusal is a refusal. The kernel never invents a parameter it was not given.
    #[test]
    fn unmodelled_parameters_are_refused_not_defaulted() {
        // matrix_coeffs 2 is literally "UNSPECIFIED" — the most tempting thing in the world to
        // default to BT.601, and the one that would silently mis-render a BT.2020 file.
        assert_eq!(params_from_vui(2, true, None), Err(YuvKernelError::UnsupportedMatrix(2)));
        assert_eq!(params_from_vui(9, true, None), Err(YuvKernelError::UnsupportedMatrix(9)));
        assert_eq!(params_from_vui(0, true, None), Err(YuvKernelError::UnsupportedMatrix(0)));
        // The vertically co-sited sitings need a second vertical phase this kernel does not have.
        assert_eq!(params_from_vui(6, true, Some(2)), Err(YuvKernelError::UnsupportedSiting(2)));
        assert_eq!(params_from_vui(6, true, Some(5)), Err(YuvKernelError::UnsupportedSiting(5)));
        // …and the ones it does model resolve exactly as the corpus reads.
        assert_eq!(
            params_from_vui(6, true, None),
            Ok(p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Left)),
            "matrix 6 + full-range + absent chroma_loc IS what every corpus HEIC declares"
        );
        assert_eq!(
            params_from_vui(5, false, Some(1)),
            Ok(p(YuvMatrix::Bt601, YuvRange::Limited, ChromaSiting::Center))
        );
        assert_eq!(
            params_from_vui(1, false, Some(0)),
            Ok(p(YuvMatrix::Bt709, YuvRange::Limited, ChromaSiting::Left))
        );
    }

    /// Malformed frames fail closed. Nothing here may panic or read out of bounds.
    #[test]
    fn malformed_frames_fail_closed() {
        let y = vec![0u8; 64];
        let uv = vec![128u8; 32];
        let pp = p(YuvMatrix::Bt601, YuvRange::Full, ChromaSiting::Left);
        assert_eq!(nv12_to_rgb8(&Nv12Frame::packed(&y, &uv, 0, 8), pp), Err(YuvKernelError::EmptyFrame));
        assert_eq!(nv12_to_rgb8(&Nv12Frame::packed(&y, &uv, 8, 0), pp), Err(YuvKernelError::EmptyFrame));
        assert!(matches!(
            nv12_to_rgb8(&Nv12Frame::packed(&y, &uv, 64, 64), pp),
            Err(YuvKernelError::ShortPlane { .. })
        ));
        assert!(matches!(
            nv12_to_rgb8(&Nv12Frame { y: &y, y_stride: 4, uv: &uv, uv_stride: 8, w: 8, h: 8 }, pp),
            Err(YuvKernelError::StrideTooSmall { plane: "Y", .. })
        ));
        assert!(matches!(
            nv12_to_rgb8(&Nv12Frame { y: &y, y_stride: 8, uv: &uv, uv_stride: 2, w: 8, h: 8 }, pp),
            Err(YuvKernelError::StrideTooSmall { plane: "UV", .. })
        ));
        assert!(matches!(
            nv12_to_rgb8(&Nv12Frame::packed(&y, &uv, 1 << 16, 8), pp),
            Err(YuvKernelError::FrameTooLarge { .. })
        ));
    }

    /// The digest is the REAL SHA-256, pinned against the published FIPS 180-4 / NIST vectors —
    /// so every number in [`GOLDEN_PINS`] is independently reproducible with `sha256sum` and no
    /// Falcon code in the loop.
    #[test]
    fn sha256_matches_an_external_oracle() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // The two-block padding path (>= 56 bytes in the tail) and the multi-block path.
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1000000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The golden table's own hygiene: unique names, sane dimensions, every fixture declared, and
    /// every axis of the parameter space actually represented. A pin set that quietly lost its
    /// BT.709 rows would still pass every digest.
    #[test]
    fn the_golden_table_covers_the_parameter_space() {
        let mut seen = std::collections::BTreeSet::new();
        for c in GOLDEN_PINS {
            assert!(seen.insert(c.name), "duplicate golden case name {}", c.name);
            assert!(c.w > 0 && c.h > 0, "{} has an empty frame", c.name);
            if let YuvGoldenSrc::Fixture(n) = c.src {
                assert!(REAL_TILE_FIXTURES.contains(&n), "{} names undeclared fixture {n}", c.name);
            }
        }
        for m in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            for r in [YuvRange::Full, YuvRange::Limited] {
                assert!(
                    GOLDEN_PINS.iter().any(|c| c.params.matrix == m && c.params.range == r),
                    "no golden row for {m:?} x {r:?}"
                );
            }
        }
        for s in [ChromaSiting::Left, ChromaSiting::Center] {
            assert!(GOLDEN_PINS.iter().any(|c| c.params.siting == s), "no golden row for {s:?}");
        }
        // Odd width, odd height, and a real tile must all be present.
        assert!(GOLDEN_PINS.iter().any(|c| c.w % 2 == 1), "no odd-WIDTH golden row");
        assert!(GOLDEN_PINS.iter().any(|c| c.h % 2 == 1), "no odd-HEIGHT golden row");
        assert!(
            GOLDEN_PINS.iter().filter(|c| matches!(c.src, YuvGoldenSrc::Fixture(_))).count() >= 2,
            "the pin set must include real decoded tiles, not only synthetic lattices"
        );
        // The lattice row must genuinely be the full (U,V) product, or its coverage claim is a lie.
        let (_, uv) = yuv_synth_nv12(YuvSynthPattern::Cube, 512, 512);
        let pairs: std::collections::BTreeSet<(u8, u8)> =
            uv.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        assert_eq!(pairs.len(), 65536, "the cube pattern must cover every (U,V) pair exactly once");
    }
}
