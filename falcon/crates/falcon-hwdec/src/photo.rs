//! v0.8.147 (E3 milestone 2) — **the whole photo**: every tile of a HEIC grid through one decoder
//! session, composited, cropped, rotated, converted and downsampled on the GPU, with the finished
//! RGB the only thing that comes back.
//!
//! # What this adds to milestone 1
//!
//! M1 decodes ONE tile and hands back NV12 in system memory. That was the risk it was built to
//! retire (does our DXVA marshalling produce the right bytes?) and it answered yes on eight tiles
//! across a matched fixture pair. What it deliberately did not do is anything a *photo* needs:
//! "It does not composite the grid, does not crop, does not rotate, does not downsample, and does
//! not convert colour. Those are the rest of E3." This module is the rest of E3's assembly half.
//!
//! # The shape, and the two things it is careful about
//!
//! ```text
//!   E1 tile_source ──▶ DecodeSession::decode_tiles_streaming ──▶ HeicAssembler::write_tile
//!                          (pipelined, bounded window)              (GPU canvas, NV12)
//!                                                                        │
//!   packed RGB8 at scale_to ◀── readback ◀── resample ◀── crop+irot ◀── E2 kernel
//! ```
//!
//! **One session, all tiles** (plan non-negotiable #1). Not a session per tile: Stage 0's whole
//! feasibility argument is that per-tile synchronous submission is slower than the CPU decoder, and
//! M1's own timing row measures the reuse. The session is sized to the file's tile geometry once
//! and reused across every tier of the same photo.
//!
//! **The tile never becomes a photo in system memory.** `decode_tiles_streaming` hands each picture
//! to a callback that writes it straight into the GPU canvas, so the resident NV12 is one tile, not
//! the 74 MB a 48 MP grid comes to. From the canvas onward nothing leaves the GPU until the final
//! RGB — the E2 kernel, the crop, the rotation and both resample passes all read and write VRAM.
//!
//! The tiles do still make ONE trip across PCIe on the way in, because M1's session reads its
//! decode surfaces back through a staging texture and "NV12-with-stride out" is its published
//! contract. That is a KNOWN and MEASURED gap rather than an oversight: the upload half costs about
//! 1 ms of the 46.4 ms this path spends decoding and uploading a 48 MP photo's 54 tiles (M1's own
//! pipelined figure for the same 54 tiles, with no upload at all, is 45.49 ms), so the whole prize
//! for closing it is bounded above by M1's readback and is a good deal less than that. Closing it
//! would need the D3D11 decode surface shared into the wgpu device through a D3D12 shared NT
//! handle, plus a plane-format step, because wgpu exposes an NV12 texture's planes as
//! `R8Unorm`/`Rg8Unorm` and E2's kernel binds `texture_2d<u32>` precisely so that no unorm fetch
//! can put a driver's 8-bit→float convert in the middle of the colour chain.
//!
//! # The contract
//!
//! Plan non-negotiable #4: packed RGB8, 24 bpp, stride `w*3`, `irot` APPLIED, un-colour-managed in
//! the file's gamut, dims byte-identical to `finish_source` per `scale_to`. The dims half is a
//! GATE, not an aspiration: `tests/hw_photo.rs` asks the shipping WIC path for its answer on every
//! corpus file at every tier the shipping path actually requests, and requires this path to land on
//! the same pair. Colour management is somebody else's job by design — the pixels come out in the
//! file's own gamut and the existing CM chain converts them, exactly as the WIC path's do.
//!
//! # …and, v0.8.165 (WAVE 1), a SECOND contract beside it
//!
//! [`PhotoDecoder::decode_managed`] finishes the same photo as RGBA8, 32 bpp, stride `w*4`, already
//! converted into the caller's output gamut. It is not a replacement: the contract above is what
//! rung 0 of `decode_heic_lane` must answer with, because rung 0 has to be substitutable for the
//! WIC rungs below it, and a frame already in the OUTPUT gamut is substitutable for nothing. The
//! managed door exists for the ONE caller that is not the ladder — the detail tier, which knows
//! both gamuts and was paying `falcon_color::transform_rgba` (110–240 ms at 48 MP) plus an
//! RGB→RGBA expansion (50–76 ms) for a conversion the finish pass can do where the pixel already
//! is. The colour arithmetic is `falcon-gpu`'s shared `rot_uv_cm_core!`, i.e. the JPEG chain's own
//! shader source; `tests/hw_photo.rs`'s cross-path gate measures the two arms against each other.
//!
//! # No longer inert
//!
//! v0.8.148 (E5) installed the hook: `falcon-native` lists this crate and `decode_heic_lane`'s
//! rung 0 calls it. (This section used to say the opposite, and said so until v0.8.164.)

use std::path::Path;
use std::sync::Arc;

use falcon_gpu::heic::{FinishOut, GridGeometry, HeicAssembler, Mirror, Nv12Tile};

use crate::session::{DecodeSession, Nv12Image, TileRun};
use crate::{tile_source, HwDecError, TileSource};

/// v0.8.171: what a photo decode PRODUCED, when the caller armed a supersession signal.
///
/// The whole reason it is a type and not an `Option` is documented on [`TileRun`]: `Superseded` is a
/// third outcome, and a caller that folds it into `None` or into `Err` will do one of the four wrong
/// things (memoise a refusal against a healthy file, count a decline against a healthy lane, stand
/// the lane down, or fall back to a software decode) for a photograph nothing is wrong with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhotoRun<T> {
    Done(T),
    /// The caller's signal went true mid-grid; `done` of `total` tiles had decoded. The session is
    /// back in the pool, clean and immediately reusable.
    Superseded { done: usize, total: usize },
}

/// The un-watched doors' impossible branch, named once so the four of them cannot word it four ways.
/// Reachable only if [`DecodeSession::decode_tiles_streaming`] reported an abort to a caller whose
/// predicate is the literal `false`.
const NO_SIGNAL_YET_SUPERSEDED: HwDecError =
    HwDecError::Unsupported("a decode with no supersession signal reported one");

/// The finished photo: the contract's buffer and its dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoRgb {
    /// Packed RGB8, 24 bpp, stride `w * 3`.
    pub rgb: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// v0.8.165 (WAVE 1): the finished photo, DISPLAY-READY — RGBA8, 32 bpp, stride `w * 4`, already
/// converted into the caller's output gamut on the assembly GPU. [`PhotoDecoder::decode_managed`]
/// is the only producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotoRgba {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// How the tiles reach the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {
    /// The product shape: submit up to `surface_count` pictures before reading any back.
    Pipelined,
    /// One picture at a time, each `Map`ped before the next is submitted. Slower by a GPU sync per
    /// tile, and kept because "the pipelined path gives the same bytes as the serialised one" is
    /// only a claim if both paths exist.
    Serialised,
}

/// The lever's one definition — private module, re-exported below at whichever visibility
/// v0.8.152's `falsifiers` gate (ruling 5.1(b)) calls for. One enum, two visibilities, no copy to
/// drift.
mod fault {
/// A deliberate defect, so the gates can be shown to bite.
///
/// The same discipline as M1's `crate::dxva::QmatrixPolicy`: the falsifier is a real code path
/// the tests drive, not a comment claiming a test would have failed. Only a falsifier ever passes
/// anything but [`AssemblyFault::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssemblyFault {
    None,
    /// Place one interior tile two luma pixels to the right of where it belongs. Two rather than
    /// one because the canvas is NV12 and an odd origin has no chroma sample to land on — this is
    /// the SMALLEST placement error the compositor can actually make, and the seam hunt has to see
    /// it.
    TileOffset,
    /// Composite and crop correctly, then forget `irot`. On a portrait file this is the classic
    /// silent defect: the picture is perfectly good and lying on its side.
    DropRotation,
    /// Turn the picture the RIGHT number of quarters the WRONG WAY (90 ↔ 270). The dims are
    /// unchanged, so this is the rotation error the dims gate cannot see and only a content check
    /// can — which is why the milestone asks for both on IMG_2814.
    ReverseRotation,
    /// Ask for an output one pixel narrower than `finish_source` would. The dims gate must redden.
    ScaleOffByOne,
}
}

/// v0.8.152 (5.1(b)): visible — and `#[doc(hidden)]` — only under the `falsifiers` feature, i.e.
/// only when a test or example target is being built. See `Cargo.toml`'s feature stanza.
#[cfg(feature = "falsifiers")]
#[doc(hidden)]
pub use fault::AssemblyFault;
/// The shipping build: the enum still exists (the decoder's own body branches on it), it simply
/// cannot be NAMED from outside this crate.
#[cfg(not(feature = "falsifiers"))]
pub(crate) use fault::AssemblyFault;

/// A decoder session plus the GPU assembly, sized for one file's tile geometry.
///
/// Holding one across several tiers of the same photo is the point: `CreateVideoDecoder` costs
/// 2–5.5 ms on this box (Stage 0) and the assembler's pipelines are compiled once.
pub struct PhotoDecoder {
    session: DecodeSession,
    asm: Arc<HeicAssembler>,
    /// `(decode+upload ms, assemble+readback ms)` of the most recent photo.
    last_split: (f64, f64),
}

impl PhotoDecoder {
    /// Build for `src`'s geometry on a GPU device of the assembly's own.
    pub fn new(src: &TileSource, surfaces: u32) -> Result<Self, HwDecError> {
        let asm = HeicAssembler::headless().map_err(|e| HwDecError::Assembly(e.to_string()))?;
        Self::with_assembler(src, surfaces, Arc::new(asm))
    }

    /// As [`Self::new`], on an assembler the caller already built.
    ///
    /// **This is E5's door.** A [`PhotoDecoder`] is per TILE GEOMETRY — a 48 MP file's 896×1024
    /// session cannot decode a 24 MP file's 640×896 tiles and says so rather than trying — so a
    /// browse holds several, while the assembler (three compiled pipelines and, in the app, the
    /// renderer's own device) should be ONE. Hence the `Arc`: sharing is the expected case and
    /// owning is the exception, not the other way round.
    pub fn with_assembler(
        src: &TileSource,
        surfaces: u32,
        asm: Arc<HeicAssembler>,
    ) -> Result<Self, HwDecError> {
        let geom = geometry(src)?;
        asm.mosaic_fits(&geom).map_err(|e| HwDecError::Assembly(e.to_string()))?;
        let session = DecodeSession::new(src.tile_w, src.tile_h, surfaces)?;
        Ok(PhotoDecoder { session, asm, last_split: (0.0, 0.0) })
    }

    pub fn assembler(&self) -> &HeicAssembler {
        &self.asm
    }

    pub fn session(&mut self) -> &mut DecodeSession {
        &mut self.session
    }

    /// How long the LAST photo spent decoding+uploading tiles versus assembling and reading back.
    /// Split rather than summed because the two halves have completely different fixes.
    pub fn last_split_ms(&self) -> (f64, f64) {
        self.last_split
    }

    /// Decode `src` whole and finish it at `scale_to` — `None` for the photo's native size.
    ///
    /// v0.8.171: this door cannot be superseded, by construction — it passes a predicate that is
    /// always false, so its [`PhotoRun`] is always `Done` and it keeps the signature every caller
    /// and every byte-pinning row since E3-M1 was written against. [`Self::decode_watched`] is the
    /// door that can stop early.
    pub fn decode(&mut self, src: &TileSource, scale_to: Option<u32>) -> Result<PhotoRgb, HwDecError> {
        match self.decode_watched(src, scale_to, &mut || false)? {
            PhotoRun::Done(p) => Ok(p),
            // Unreachable: the predicate above is a constant `false`. Stated as an error rather than
            // an `unreachable!()` because a panic inside a decode worker is a worse answer than an
            // error, on a branch that exists only to satisfy the match.
            PhotoRun::Superseded { .. } => Err(NO_SIGNAL_YET_SUPERSEDED),
        }
    }

    /// v0.8.171 (HEIC SPEED PRIORITY) — [`Self::decode`] that may STOP EARLY.
    ///
    /// `superseded` is asked between tile chunks (see [`DecodeSession::decode_tiles_streaming`]) and
    /// a `true` abandons the photo: the session is checked back in clean, the canvas is dropped, and
    /// nothing downstream of the assembly runs at all — no NV12→RGB pass over the mosaic, no crop,
    /// no rotate, no resample, no readback. The caller gets [`PhotoRun::Superseded`], which is NOT a
    /// failure and NOT a decline; see [`TileRun`] for what it costs a caller who confuses them.
    pub fn decode_watched(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        superseded: &mut dyn FnMut() -> bool,
    ) -> Result<PhotoRun<PhotoRgb>, HwDecError> {
        self.decode_impl(
            src,
            scale_to,
            Submission::Pipelined,
            AssemblyFault::None,
            FinishOut::SourceRgb,
            superseded,
        )
    }

    /// v0.8.165 (WAVE 1) — [`Self::decode`] finishing to **display-ready RGBA in `dst`**.
    ///
    /// Same decode, same composite, same E2 kernel, same crop/`irot`/resample: the ONLY difference
    /// is that the pass which was already writing the finished pixel also converts its gamut and
    /// writes an alpha. The returned buffer is 32 bpp at stride `w*4` and its `rgb` field therefore
    /// holds RGBA — which is why this is a separate door rather than a flag on `decode`: the name
    /// on the shipped contract's struct must keep meaning what it says.
    ///
    /// `src_gamut` is the caller's own source-gamut answer for this file (`shot_source_gamut` in
    /// the app), NOT a second reading of the container — one probe, one answer, no L42 seam.
    pub fn decode_managed(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        src_gamut: falcon_color::Gamut,
        dst: falcon_color::Gamut,
    ) -> Result<PhotoRgba, HwDecError> {
        match self.decode_managed_watched(src, scale_to, src_gamut, dst, &mut || false)? {
            PhotoRun::Done(p) => Ok(p),
            PhotoRun::Superseded { .. } => Err(NO_SIGNAL_YET_SUPERSEDED),
        }
    }

    /// v0.8.171 — [`Self::decode_managed`] that may STOP EARLY. See [`Self::decode_watched`].
    pub fn decode_managed_watched(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        src_gamut: falcon_color::Gamut,
        dst: falcon_color::Gamut,
        superseded: &mut dyn FnMut() -> bool,
    ) -> Result<PhotoRun<PhotoRgba>, HwDecError> {
        Ok(match self.decode_impl(
            src,
            scale_to,
            Submission::Pipelined,
            AssemblyFault::None,
            FinishOut::ManagedRgba { src: src_gamut, dst },
            superseded,
        )? {
            PhotoRun::Done(p) => PhotoRun::Done(PhotoRgba { rgba: p.rgb, w: p.w, h: p.h }),
            PhotoRun::Superseded { done, total } => PhotoRun::Superseded { done, total },
        })
    }

    /// [`Self::decode`] with the submission shape and any deliberate defect under explicit control.
    ///
    /// v0.8.152 (5.1(b)): behind the `falsifiers` feature, so this door exists for test and example
    /// targets and does NOT exist in the library `falcon-native` links — "a shipping caller reached
    /// for a falsifier" is now a compile error.
    #[cfg(feature = "falsifiers")]
    #[doc(hidden)]
    pub fn decode_with(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        how: Submission,
        fault: AssemblyFault,
    ) -> Result<PhotoRgb, HwDecError> {
        self.decode_done(src, scale_to, how, fault, FinishOut::SourceRgb)
    }

    /// v0.8.165: [`Self::decode_with`] with the OUTPUT CONTRACT under explicit control too — the
    /// cross-path fidelity gate needs the SAME canvas finished BOTH ways to have anything to
    /// compare, and it must reach the falsifier (`AssemblyFault`) doors as well.
    #[cfg(feature = "falsifiers")]
    #[doc(hidden)]
    pub fn decode_with_out(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        how: Submission,
        fault: AssemblyFault,
        out: FinishOut,
    ) -> Result<PhotoRgb, HwDecError> {
        self.decode_done(src, scale_to, how, fault, out)
    }

    /// The falsifier doors' shared un-watched call: no supersession signal, so the run always
    /// completes and the [`PhotoRun`] wrapper is unwrapped here rather than at each door.
    #[cfg(feature = "falsifiers")]
    fn decode_done(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        how: Submission,
        fault: AssemblyFault,
        out: FinishOut,
    ) -> Result<PhotoRgb, HwDecError> {
        match self.decode_impl(src, scale_to, how, fault, out, &mut || false)? {
            PhotoRun::Done(p) => Ok(p),
            PhotoRun::Superseded { .. } => Err(NO_SIGNAL_YET_SUPERSEDED),
        }
    }

    fn decode_impl(
        &mut self,
        src: &TileSource,
        scale_to: Option<u32>,
        how: Submission,
        fault: AssemblyFault,
        out: FinishOut,
        superseded: &mut dyn FnMut() -> bool,
    ) -> Result<PhotoRun<PhotoRgb>, HwDecError> {
        let mut geom = geometry(src)?;
        if fault == AssemblyFault::DropRotation {
            geom.rot_quarters = 0;
            geom.mirror = None;
        }
        if fault == AssemblyFault::ReverseRotation {
            geom.rot_quarters = (4 - geom.rot_quarters) % 4;
        }
        let params = src.yuv_params()?;
        let canvas =
            self.asm.canvas(geom).map_err(|e| HwDecError::Assembly(e.to_string()))?;

        // ── decode + composite ──
        let t0 = std::time::Instant::now();
        {
            let asm = &self.asm;
            let place = |k: usize, img: Nv12Image| -> Result<(), HwDecError> {
                let (mut x, y) = src.tile_origin(k);
                // The seam falsifier: ONE interior tile, two luma columns to the right. Interior
                // rather than tile 0 so the error is a seam and not a border.
                if fault == AssemblyFault::TileOffset && k == src.tiles.len() / 2 {
                    x += 2;
                }
                let tile = Nv12Tile {
                    data: &img.data,
                    stride: img.stride,
                    w: img.width,
                    h: img.height,
                };
                asm.write_tile(&canvas, &tile, x, y)
                    .map_err(|e| HwDecError::Assembly(e.to_string()))
            };
            match how {
                Submission::Pipelined => {
                    // v0.8.171: an ABORT returns from here with the canvas still a local. Dropping
                    // it is the whole of the cleanup — it is created per call (never cached, never
                    // shared, nothing outside this frame can observe it), so a partially written
                    // mosaic costs its own VRAM until the assembly device's next poll and costs
                    // correctness nothing. What the early return SKIPS is the expensive half:
                    // `finish_out`'s NV12→RGB kernel over the whole mosaic, the crop, the rotate,
                    // the resample, and the banded readback across the bus.
                    match self.session.decode_tiles_streaming(
                        &src.params,
                        &src.tiles,
                        place,
                        superseded,
                    )? {
                        TileRun::Complete => {}
                        TileRun::Aborted { done, total } => {
                            return Ok(PhotoRun::Superseded { done, total })
                        }
                    }
                }
                Submission::Serialised => {
                    for (k, item) in src.tiles.iter().enumerate() {
                        let img = self.session.decode_tile(&src.params, item)?;
                        place(k, img)?;
                    }
                }
            }
        }
        let decode_ms = t0.elapsed().as_secs_f64() * 1e3;

        // ── convert + crop + rotate + resample + the one readback ──
        let t1 = std::time::Instant::now();
        let (mut ow, oh) = HeicAssembler::output_dims(&geom, scale_to);
        if fault == AssemblyFault::ScaleOffByOne {
            ow = ow.saturating_sub(1).max(1);
        }
        let (rgb, w, h) = self
            .asm
            .finish_out(&canvas, params.siting, params.coeffs(), ow, oh, out)
            .map_err(|e| HwDecError::Assembly(e.to_string()))?;
        self.last_split = (decode_ms, t1.elapsed().as_secs_f64() * 1e3);
        Ok(PhotoRun::Done(PhotoRgb { rgb, w, h }))
    }
}

/// E1's geometry, in the assembly's own words. The one place `irot` degrees become quarter-turns
/// and `imir`'s axis becomes an axis — both conversions are total, and both are the kind that get
/// written twice and then disagree, so they are written here and nowhere else.
pub fn geometry(src: &TileSource) -> Result<GridGeometry, HwDecError> {
    let rot_quarters = match src.irot {
        0 => 0,
        90 => 1,
        180 => 2,
        270 => 3,
        _ => return Err(HwDecError::Unsupported("irot is not a quarter turn")),
    };
    let mirror = src.imir.map(|m| match m {
        falcon_decode::HeifMirror::Vertical => Mirror::Vertical,
        falcon_decode::HeifMirror::Horizontal => Mirror::Horizontal,
    });
    let g = GridGeometry {
        mosaic_w: src.mosaic.0,
        mosaic_h: src.mosaic.1,
        crop_x: src.crop.x,
        crop_y: src.crop.y,
        crop_w: src.crop.w,
        crop_h: src.crop.h,
        rot_quarters,
        mirror,
    };
    // E1 already computed the display dims from the same three facts. If these two ever disagree,
    // one of them is wrong and guessing which would be the whole L42 mistake again.
    if g.display() != src.display {
        return Err(HwDecError::Bitstream("the assembly's display dims disagree with E1's"));
    }
    Ok(g)
}

/// The inverse of everything pass A's gather does, in Rust: DISPLAY pixel → which grid tile it came
/// from.
///
/// It exists so a diagnostic can attribute a bad pixel to a TILE. That is the whole seam hunt: a
/// compositor's edge arithmetic goes wrong for one tile, a whole-image mean over 48 Mpx buries it,
/// and per-tile means do not. It is also the one piece of the rotation map written twice (here and
/// in `FINISH_WGSL`'s `mosaic_index`) — deliberately, because the test that uses it is checking the
/// shader's geometry and a helper that shared the shader's arithmetic could not.
///
/// Built once and asked millions of times, hence a struct rather than a free function that would
/// re-derive and re-validate the geometry per pixel.
#[derive(Debug, Clone, Copy)]
pub struct DisplayMap {
    geom: GridGeometry,
    display: (u32, u32),
    tile_w: u32,
    tile_h: u32,
    cols: u32,
}

impl DisplayMap {
    pub fn new(src: &TileSource) -> Result<Self, HwDecError> {
        let geom = geometry(src)?;
        Ok(DisplayMap {
            geom,
            display: geom.display(),
            tile_w: src.tile_w.max(1),
            tile_h: src.tile_h.max(1),
            cols: src.cols.max(1),
        })
    }

    pub fn display_dims(&self) -> (u32, u32) {
        self.display
    }

    /// The MOSAIC pixel that display pixel `(u, v)` came from — the exact gather `FINISH_WGSL`'s
    /// `mosaic_index` computes, written independently in Rust.
    pub fn mosaic_at(&self, u: u32, v: u32) -> (u32, u32) {
        let g = &self.geom;
        let (dw, dh) = self.display;
        let (mut a, mut b) = (u.min(dw - 1) as i64, v.min(dh - 1) as i64);
        match g.mirror {
            Some(Mirror::Vertical) => a = dw as i64 - 1 - a,
            Some(Mirror::Horizontal) => b = dh as i64 - 1 - b,
            None => {}
        }
        let (cw, ch) = (g.crop_w as i64, g.crop_h as i64);
        let (x, y) = match g.rot_quarters {
            1 => (cw - 1 - b, a),
            2 => (cw - 1 - a, ch - 1 - b),
            3 => (b, ch - 1 - a),
            _ => (a, b),
        };
        (
            (g.crop_x as i64 + x.clamp(0, cw - 1)) as u32,
            (g.crop_y as i64 + y.clamp(0, ch - 1)) as u32,
        )
    }

    /// `(row, col)` of the tile that supplied display pixel `(u, v)`.
    pub fn tile_at(&self, u: u32, v: u32) -> (u32, u32) {
        let (mx, my) = self.mosaic_at(u, v);
        (my / self.tile_h, mx / self.tile_w)
    }

    /// The flat cell index `row * cols + col`.
    pub fn cell_at(&self, u: u32, v: u32) -> usize {
        let (r, c) = self.tile_at(u, v);
        (r * self.cols + c) as usize
    }
}

/// The dims the contract demands for this file at `scale_to`, without decoding anything.
///
/// E5's router wants this before it commits to a lane, and the dims gate wants it as the thing it
/// compares against the shipping path.
pub fn output_dims(src: &TileSource, scale_to: Option<u32>) -> Result<(u32, u32), HwDecError> {
    Ok(HeicAssembler::output_dims(&geometry(src)?, scale_to))
}

/// One photo, one call: parse, decode, assemble. The convenience door; a caller decoding several
/// tiers of the same file should hold a [`PhotoDecoder`] instead and pay for the session once.
pub fn decode_photo(path: &Path, scale_to: Option<u32>) -> Result<PhotoRgb, HwDecError> {
    let src = tile_source(path)?;
    let mut d = PhotoDecoder::new(&src, 8)?;
    d.decode(&src, scale_to)
}
