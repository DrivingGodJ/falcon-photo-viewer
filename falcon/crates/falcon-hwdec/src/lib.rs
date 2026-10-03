//! v0.8.146 — **E3 milestone 1: the DXVA marshalling, retired as a risk.**
//!
//! The hardware-decode epic's plan (July 2026) names
//! route B′ — direct D3D11VA through `ID3D11VideoDevice` — as the product backend, and names
//! "DXVA marshalling complexity" as one of the risks it carries. Stage 0 measured everything AROUND
//! that risk and could not touch the risk itself: it proved the profile is exposed, that
//! `CreateVideoDecoder` costs 2–5.5 ms, and that the per-picture API round trip costs 0.09 ms — but
//! its own note says the picture-parameter and slice-control payloads "are NOT marshalled (that is
//! E3's job), so this is the API round-trip FLOOR, not a decode". The investigation's carried-forward
//! item #4 asked for exactly one thing next: *"one tile decoded correctly through
//! `ID3D11VideoDevice`, byte-compared against NVDEC"*.
//!
//! This crate is that, and only that. It decodes ONE HEVC Main Still Picture tile at a time, from
//! the byte ranges E1 (`falcon_decode::heif_grid`) reports, through a decoder session of our own on
//! a dedicated D3D11 device, and hands back NV12 — the exact input E2's kernel takes. It does not
//! composite the grid, does not crop, does not rotate, does not downsample, and does not convert
//! colour. Those are the rest of E3.
//!
//! # INERT
//!
//! Nothing that ships depends on this crate. `falcon-native` does not list it; there is no
//! environment flag, because there is nothing to switch on. It is a workspace member so that
//! `cargo test --workspace` gates it, and its tests run for real on hardware that has a video
//! device and SKIP WITH A NAMED REASON on hardware that does not — never `cfg`'d out, because a
//! test that cannot fail is not a gate.
//!
//! # Where every number comes from
//!
//! * The DXVA structure layout: the Windows SDK's own `um/dxva.h`, cross-checked by compiling it
//!   with MSVC and printing `sizeof`/`offsetof` (see [`dxva`]).
//! * The field semantics: ffmpeg's `libavcodec/dxva2_hevc.c`, read as a reference. **No ffmpeg code
//!   is linked, vendored or translated** — the patent shield (plan non-negotiable #3) is "consume
//!   the OS/driver decoder", and `ID3D11VideoDevice` is that.
//! * Every parameter value: the file's own `hvcC`, parsed by [`hevc`] down to the bit. L42 —
//!   never decide from a name when the answering bytes are in hand.
//!
//! # The one thing that will bite the next reader
//!
//! `scaling_list_enabled_flag` is 1 on every corpus HEIC. When the SPS does not additionally carry
//! `scaling_list_data()`, the applicable matrices are the H.265 **defaults** — graded matrices, not
//! flat 16s — and when it does, they are 20 explicit ones. Both must reach the driver through
//! `DXVA_Qmatrix_HEVC`, in the signalled (up-right diagonal) order, or the picture is wrong and
//! nothing says so. The matched fixture pair IMG_1826 (defaults) / IMG_3258 (explicit) exists for
//! this and both are pinned.

#![cfg_attr(not(windows), allow(dead_code))]

pub mod hevc;

#[cfg(windows)]
pub mod dxva;
#[cfg(windows)]
pub mod photo;
#[cfg(windows)]
pub mod session;

#[cfg(windows)]
pub use photo::{PhotoDecoder, PhotoRgb, PhotoRgba, PhotoRun, Submission};
/// v0.8.152 (5.1(b)): the deliberate-defect lever rides the `falsifiers` feature — see
/// `Cargo.toml`. Re-exported at the crate root only when that feature is on, so the shipping
/// library has no such name.
#[cfg(all(windows, feature = "falsifiers"))]
#[doc(hidden)]
pub use photo::AssemblyFault;
#[cfg(windows)]
pub use session::{take_readback_note, DecodeDevice, DecodeSession, Nv12Image, SurfaceLease, TileRun};

use std::path::Path;

/// Every way this stage can decline. All of them are declines: none is a panic, and none is a guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HwDecError {
    /// The bytes are not the HEVC syntax they claim to be. The string names WHICH read failed.
    Bitstream(&'static str),
    /// A syntax branch this stage deliberately does not model, NAMED rather than approximated.
    Unsupported(&'static str),
    /// No hardware D3D11 device with video support on this box.
    NoVideoDevice,
    /// A video device exists but does not decode HEVC Main to NV12.
    NoHevcProfile,
    /// A D3D11 call failed. `hr` is the raw HRESULT so a tester's report can carry the number.
    Api { call: &'static str, hr: i32 },
    /// The driver's buffer for a fixed-size payload was smaller than the payload.
    BufferTooSmall { need: usize, have: usize },
    /// The picture's slices do not fit the driver's bitstream buffer.
    BitstreamTooLarge { need: usize, have: usize },
    /// Every decode surface is still in flight.
    SurfacesExhausted,
    /// The device was removed or reset. Latched: the session refuses everything afterwards.
    DeviceLost,
    /// The container could not be read (delegated to E1, whose message is carried whole).
    Container(String),
    /// v0.8.147 (E3-M2): the GPU assembly declined — no adapter, a mosaic past this device's
    /// limits, a tile that will not fit its canvas. Carried whole for the same reason
    /// [`HwDecError::Container`] is: the sentence names WHICH limit, and a fall-soft log that says
    /// "the mosaic's RGB is 198180864 bytes, over this device's 134217728 byte storage binding
    /// limit" is worth a great deal more than a fixed key.
    Assembly(String),
    /// v0.8.149 (F7): the picture is not 8-bit. `where` names WHICH of the three declarations said
    /// so, `bits` is what it said. Its own variant rather than an [`HwDecError::Unsupported`]
    /// string because this is the one refusal whose ABSENCE would be silent: an 8-bit
    /// `HEVC_VLD_MAIN` session handed a Main10 bitstream does not error, it produces a wrong
    /// picture, and every gate downstream (dims, seams, the CM chain) would pass it.
    BitDepth { source: &'static str, bits: u8 },
}

impl std::fmt::Display for HwDecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HwDecError::Bitstream(w) => write!(f, "bitstream: {w}"),
            HwDecError::Unsupported(w) => write!(f, "unsupported: {w}"),
            HwDecError::NoVideoDevice => write!(f, "no D3D11 device with video support"),
            HwDecError::NoHevcProfile => write!(f, "driver does not decode HEVC Main to NV12"),
            HwDecError::Api { call, hr } => write!(f, "{call} failed (hr 0x{:08X})", *hr as u32),
            HwDecError::BufferTooSmall { need, have } => {
                write!(f, "driver buffer holds {have} bytes, payload is {need}")
            }
            HwDecError::BitstreamTooLarge { need, have } => {
                write!(f, "picture needs {need} bitstream bytes, driver offers {have}")
            }
            HwDecError::SurfacesExhausted => write!(f, "every decode surface is in flight"),
            HwDecError::DeviceLost => write!(f, "the decode device was removed"),
            HwDecError::Container(e) => write!(f, "container: {e}"),
            HwDecError::Assembly(e) => write!(f, "assembly: {e}"),
            HwDecError::BitDepth { source, bits } => write!(
                f,
                "the {source} declares {bits}-bit samples; this lane marshals 8-bit HEVC Main only"
            ),
        }
    }
}

/// v0.8.149 (F7) — **THE BIT-DEPTH GATE, as a pure function of the three declarations.**
///
/// # Why this exists at all
///
/// E5 shipped nine ways to decline and not one of them was the bit depth. The session is created
/// for `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN` — Main, 8-bit — and the surfaces are `NV12`, which is
/// 8-bit by definition. Feed that a Main10 bitstream and the failure is the WORST shape there is:
/// the driver does not have to refuse it, the returned surface is still `NV12` at still the right
/// dimensions, so E3-M2's dims gate passes, the seam statistic passes, the CM chain passes, and
/// the user gets a quietly wrong photo. Every other decline in this lane is a fall-soft; this one
/// is the difference between a fall-soft and a corruption.
///
/// # Why THREE sources and why they must agree
///
/// L42 — never decide from a name when the answering bytes are in hand — and here there are three
/// sets of answering bytes, written by three different producers:
///
/// * `pixi` — E1's, from the container's own `ItemPropertyContainer`. It is what a MUXER wrote.
/// * the `hvcC` record's `bit_depth_luma_minus8`/`chroma` — what the CONFIGURATION says.
/// * the SPS's own `bit_depth_luma_minus8`/`chroma` — what the BITSTREAM says, i.e. what the
///   hardware will actually decode.
///
/// The SPS is the authority, so a reader might reasonably ask why the other two are consulted. The
/// answer is that they are cheaper (the `pixi` arm runs before a single NAL is parsed, which is
/// what "BEFORE marshalling" means) and that a DISAGREEMENT is itself a refusal: a file whose
/// container and whose bitstream describe different pictures is not a file this lane should be
/// guessing about. Every corpus file agrees on 8 in all three.
///
/// `pixi` is `Option` because the box is optional in the container; absent is not a licence to
/// assume, it is simply one fewer witness, and the SPS still has to say 8.
pub fn bit_depth_gate(
    pixi_first_channel: Option<u8>,
    record_luma: u8,
    record_chroma: u8,
    sps_luma: u8,
    sps_chroma: u8,
) -> Result<(), HwDecError> {
    if let Some(bits) = pixi_first_channel {
        if bits != 8 {
            return Err(HwDecError::BitDepth { source: "container's pixi", bits });
        }
    }
    for (source, bits) in [
        ("hvcC record's luma depth", record_luma),
        ("hvcC record's chroma depth", record_chroma),
        ("SPS luma depth", sps_luma),
        ("SPS chroma depth", sps_chroma),
    ] {
        if bits != 8 {
            return Err(HwDecError::BitDepth { source, bits });
        }
    }
    Ok(())
}

impl std::error::Error for HwDecError {}

/// One file's tiles, ready to feed a session: the parameter sets, the geometry, and the tile
/// payloads as owned bytes.
///
/// This is the seam between E1 and E3. E1 says where the bytes are; this reads them once and hands
/// over a shape a decoder can consume, so nothing downstream re-parses a container.
///
/// v0.8.147 (E3-M2): it now also carries the geometry the ASSEMBLY needs — the mosaic, the crop,
/// `irot`/`imir` — because those come off the same single parse and re-reading the container to
/// learn them would be exactly the drift this seam exists to prevent.
#[derive(Debug, Clone)]
pub struct TileSource {
    pub params: hevc::ParameterSets,
    pub tile_w: u32,
    pub tile_h: u32,
    pub rows: u32,
    pub cols: u32,
    /// Per-tile item data in grid raster order — the length-prefixed NAL stream, verbatim.
    pub tiles: Vec<Vec<u8>>,
    /// `cols × tile_w` by `rows × tile_h` — the canvas the tiles compose onto.
    pub mosaic: (u32, u32),
    /// The rect of the mosaic that survives the grid trim and any `clap`, PRE-rotation.
    pub crop: falcon_decode::HeifRect,
    /// `irot` in degrees COUNTER-CLOCKWISE: 0, 90, 180 or 270.
    pub irot: u16,
    pub imir: Option<falcon_decode::HeifMirror>,
    /// Post-crop, POST-rotation — the dimensions the shipping WIC path answers with.
    pub display: (u32, u32),
}

impl TileSource {
    /// Where tile `k` (grid raster order) lands in the mosaic, in luma pixels.
    pub fn tile_origin(&self, k: usize) -> (u32, u32) {
        let cols = self.cols.max(1) as usize;
        (((k % cols) as u32) * self.tile_w, ((k / cols) as u32) * self.tile_h)
    }

    /// The YUV parameters this file's SPS VUI declares, or the named refusal E2's kernel gives.
    ///
    /// L42: the answering bytes are the VUI's, and `params_from_vui` refuses everything it does not
    /// model rather than defaulting — including `matrix_coeffs` 2 (`UNSPECIFIED`), which is not a
    /// licence to pick one. A file that lands here must fall soft to WIC, which is why the failure
    /// is carried as an `Unsupported` with the kernel's own sentence rather than swallowed.
    pub fn yuv_params(&self) -> Result<falcon_decode::yuv_kernel::YuvParams, HwDecError> {
        let v = &self.params.sps.vui;
        falcon_decode::yuv_kernel::params_from_vui(
            v.matrix_coeffs,
            v.video_full_range_flag,
            v.chroma_loc_info_present.then_some(v.chroma_sample_loc_type_top_field as u8),
        )
        .map_err(|e| match e {
            falcon_decode::yuv_kernel::YuvKernelError::UnsupportedMatrix(_) => {
                HwDecError::Unsupported("the VUI names a matrix the Falcon YUV kernel does not model")
            }
            falcon_decode::yuv_kernel::YuvKernelError::UnsupportedSiting(_) => {
                HwDecError::Unsupported("the VUI names a chroma siting the Falcon YUV kernel does not model")
            }
            _ => HwDecError::Unsupported("the VUI is not a shape the Falcon YUV kernel takes"),
        })
    }
}

/// `bit_depth_luma_minus8 + 8`, saturating — the SPS field is a `ue(v)` and a crafted file can put
/// any `u32` in it. Saturating rather than wrapping so a hostile value lands on 255 (which the gate
/// refuses) instead of wrapping round to 8 (which it would accept).
fn sps_depth(minus8: u32) -> u8 {
    minus8.saturating_add(8).min(u8::MAX as u32) as u8
}

/// Read a HEIC's primary grid through E1 and collect its tile payloads.
///
/// Refuses, rather than guessing, any file whose tiles do not all share one `hvcC`: SP2 measured
/// that they do on every file in the corpus, and a file that split its parameter sets would need
/// per-tile decoder state this milestone does not build.
pub fn tile_source(path: &Path) -> Result<TileSource, HwDecError> {
    let bytes = std::fs::read(path).map_err(|e| HwDecError::Container(e.to_string()))?;
    let plan = falcon_decode::parse_heif_grid(&bytes)
        .map_err(|e| HwDecError::Container(e.to_string()))?;
    let hvcc = plan
        .hvcc
        .as_deref()
        .ok_or(HwDecError::Unsupported("the file's tiles do not share one hvcC"))?;
    // v0.8.149 (F7): E1's `pixi` FIRST — before a single NAL is parsed, let alone marshalled. A
    // Main10-primary HEIC must never reach `CreateVideoDecoder(HEVC_VLD_MAIN)`, and the container
    // has already said what it is.
    if let Some(bits) = plan.bit_depth {
        if bits != 8 {
            return Err(HwDecError::BitDepth { source: "container's pixi", bits });
        }
    }
    let params = hevc::parse_hvcc(hvcc)?;
    // v0.8.152 (R3-M1) — **THE PARSER'S OWN SELF-CHECK, CONSULTED.**
    //
    // [`hevc`]'s header sells `tail_verified` as the load-bearing safety property: "a parser that
    // mis-reads one `ue(v)` in the middle almost always desynchronises the tail, so this one check
    // catches the class of error that would otherwise be invisible until the pixels came back
    // subtly wrong". Until this line it was COMPUTED AND NEVER READ — no reader anywhere in `src/`
    // — so a parameter set this parser had already declared desynchronised was marshalled into
    // `DXVA_PicParams_HEVC` and handed to the driver, which returns a plausible picture from wrong
    // parameters with no error, no decline and no fall-soft to WIC. That is precisely the failure
    // this crate says it exists to disarm (see the crate header: "or the picture is wrong and
    // nothing says so").
    //
    // It costs nothing on real files: `dxva_marshalling.rs` asserts both flags true on both corpus
    // fixtures, and the whole 34-test crate suite — including every hardware byte-identity test —
    // is green with the gate in. It is proven to bite by inverting it: with both flags forced false
    // the corpus DECLINES on `tile_source` before a single NAL reaches the driver.
    if !params.sps.tail_verified || !params.pps.tail_verified {
        return Err(HwDecError::Bitstream("parameter set did not end at rbsp_trailing_bits"));
    }
    // …and then the two declarations that come out of the bitstream itself, which is what the
    // hardware will actually see. See [`bit_depth_gate`] for why all three are asked.
    bit_depth_gate(
        plan.bit_depth,
        params.record_bit_depth_luma,
        params.record_bit_depth_chroma,
        sps_depth(params.sps.bit_depth_luma_minus8),
        sps_depth(params.sps.bit_depth_chroma_minus8),
    )?;
    if params.sps.width != plan.tile_w || params.sps.height != plan.tile_h {
        return Err(HwDecError::Bitstream("the SPS and the tile ispe disagree about the tile size"));
    }
    let mut tiles = Vec::with_capacity(plan.tiles.len());
    for t in &plan.tiles {
        let mut item = Vec::new();
        for e in &t.extents {
            let (start, end) = (e.offset as usize, e.end() as usize);
            let slice = bytes
                .get(start..end)
                .ok_or(HwDecError::Container("tile extent outside the file".into()))?;
            item.extend_from_slice(slice);
        }
        tiles.push(item);
    }
    Ok(TileSource {
        params,
        tile_w: plan.tile_w,
        tile_h: plan.tile_h,
        rows: plan.rows,
        cols: plan.cols,
        tiles,
        mosaic: plan.mosaic,
        crop: plan.crop,
        irot: plan.irot,
        imir: plan.imir,
        display: plan.display,
    })
}
