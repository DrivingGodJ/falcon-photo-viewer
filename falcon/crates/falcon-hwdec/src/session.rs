//! v0.8.146 (E3-M1) — **the D3D11VA decode session**: a dedicated device, one reusable
//! `ID3D11VideoDecoder`, and a surface pool whose lifetime this crate owns explicitly.
//!
//! # The dedicated device (plan risk: device-lock contention)
//!
//! The decoder is created on a D3D11 device of its own, made with
//! `D3D11_CREATE_DEVICE_VIDEO_SUPPORT` and never shared with the wgpu renderer. Stage 0's SP3 row
//! proved the two coexist ("renderer-style device + dedicated VIDEO_SUPPORT device — both created,
//! distinct") and measured the engine-contention cost at +0.11 ms of renderer frame time; this is
//! the structure that keeps it that way. `ID3D11VideoContext` is not free-threaded, so a
//! [`DecodeSession`] is `!Sync` by construction — it holds COM interfaces and takes `&mut self` for
//! every submission, so the compiler enforces one caller at a time on one session.
//!
//! # Surface lifetime is OURS
//!
//! Stage 0 obstacle #4: the unmarshalled API-floor probe "began failing partway at 512×512 once
//! surfaces recycled without real decodes completing", and recorded that E3 owns surface
//! recycling — "the driver will not rescue it". So a surface here is not free when
//! `DecoderEndFrame` returns; it is free when the readback that CONSUMES it has completed. The pool
//! marks a slice in flight at `DecoderBeginFrame` and only returns it after `Map`/`Unmap` on the
//! staging copy, which is a hard GPU sync point. [`SurfaceLease`] makes that ordering a type rather
//! than a convention, and `DecoderEndFrame` rides a guard so no early return can skip it.
//!
//! # Fail closed
//!
//! Every API call is checked. A failed `DecoderBeginFrame` releases nothing and returns `Err` with
//! the HRESULT; a failed submit still ends the frame; and once the device reports removed the
//! session latches [`HwDecError::DeviceLost`] and refuses everything afterwards rather than issuing
//! more work at a dead device.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;

use crate::dxva::{self, QmatrixPolicy};
use crate::hevc::ParameterSets;
use crate::HwDecError;

fn api(call: &'static str, e: windows::core::Error) -> HwDecError {
    HwDecError::Api { call, hr: e.code().0 }
}

/// v0.8.153 (skeptic A / O2) — the ONE observation this crate makes that is neither an error nor a
/// picture, and the whole reason it exists.
///
/// This crate has no logger: everything it wants to say it says by returning `Err`, and the native
/// layer writes the falcon.log line. `readback`'s `DepthPitch` check has a case that is neither —
/// a driver that reports the LUMA slice extent rather than the whole NV12 mapping is CONFORMANT and
/// must not decline, but it is exactly the fact a "the photos come back wrong on vendor X" report
/// would need, and this crate's entire evidence base for planar `DepthPitch` is one vendor.
///
/// So: latched once per process, drained once by [`take_readback_note`], and silent forever after.
/// A per-decode line would be noise on a lane that decodes every photo in a folder.
static READBACK_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static READBACK_NOTE_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn note_once(s: String) {
    if !READBACK_NOTE_SET.swap(true, std::sync::atomic::Ordering::Relaxed) {
        *READBACK_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }
}

/// Take the session's readback note, if one was left. `Some` at most ONCE per process — the caller
/// logs it and every later call answers `None`. See [`READBACK_NOTE`].
pub fn take_readback_note() -> Option<String> {
    if !READBACK_NOTE_SET.load(std::sync::atomic::Ordering::Relaxed) {
        return None; // the overwhelmingly common path: no lock, no allocation
    }
    READBACK_NOTE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// One decoded picture, exactly as the driver wrote it: NV12, **pitched**.
///
/// E2 pinned pitched surfaces as answer-neutral (`a_pitched_surface_matches_the_packed_one`) and
/// takes the stride rather than forcing a repack, so the stride is carried here rather than
/// flattened away. [`Nv12Image::packed`] produces the `stride == width` form the byte-compare uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12Image {
    pub width: u32,
    pub height: u32,
    /// Bytes per row of BOTH planes, as the driver's staging map reported it.
    pub stride: u32,
    /// `stride × height` bytes of luma followed by `stride × ceil(height/2)` bytes of interleaved
    /// chroma — the D3D11 NV12 staging layout.
    pub data: Vec<u8>,
}

impl Nv12Image {
    /// The packed (`stride == width`) NV12 buffer: `w·h` luma then `w·ceil(h/2)` chroma.
    pub fn packed(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let ch = h.div_ceil(2);
        let s = self.stride as usize;
        let mut out = Vec::with_capacity(w * h + w * ch);
        for row in 0..h {
            let o = row * s;
            out.extend_from_slice(&self.data[o..o + w]);
        }
        for row in 0..ch {
            let o = (h + row) * s;
            out.extend_from_slice(&self.data[o..o + w]);
        }
        out
    }
}

/// v0.8.171 (HEIC SPEED PRIORITY) — how a tile run ENDED, and it is deliberately not an error.
///
/// A 48 MP HEIC is ~190 tiles submitted one after another through ONE video engine, and on an iGPU
/// that run is 330–1100 ms. The 08-06 laptop logs caught the waste plainly: `full-res #91 skipped
/// (post-decode: current is 95)` — a decode that ran to completion for a photograph the user had
/// already browsed past, holding a decoder session the shot on screen was queuing for. A tile run
/// CAN stop between tiles; nothing about the format or the driver requires it to finish.
///
/// **ABORTED IS A THIRD OUTCOME AND MUST STAY ONE.** It is not `Err`: nothing failed, the session is
/// clean, the file is fine, and the caller must not memoise a refusal, stand the lane down, count a
/// decline, or fall back to a software decode — every one of which is the correct response to an
/// `Err` and the wrong response to this. It is not `Ok(picture)` either: there is no picture. The
/// shot simply has no frame yet, and the next ask for it starts fresh.
///
/// `done`/`total` are carried for the log line and nothing else — the count is what makes "it
/// stopped early" a measurement rather than an assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileRun {
    /// Every tile decoded and was handed to the callback.
    Complete,
    /// The caller's supersession signal went true at a chunk boundary; `done` tiles of `total` had
    /// been delivered. Every surface is back in the pool and the session is immediately reusable.
    Aborted { done: usize, total: usize },
}

/// A surface slice checked out of the pool. Holding one is the proof that the slice is not being
/// written by an earlier, still-in-flight decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceLease {
    pub index: u8,
}

/// The dedicated decode device. Cheap to hold, expensive to make (~2-5.5 ms for the decoder on this
/// box, per Stage 0), which is why E3 proper will pool them.
pub struct DecodeDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
}

impl DecodeDevice {
    /// Create a hardware D3D11 device with video support. Returns [`HwDecError::NoVideoDevice`] when
    /// the box has no such device — the honest "skip with a named reason" answer.
    pub fn new() -> Result<Self, HwDecError> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: every out-parameter is a live local; the call is the documented D3D11 entry point.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|_| HwDecError::NoVideoDevice)?;
        let device = device.ok_or(HwDecError::NoVideoDevice)?;
        let context = context.ok_or(HwDecError::NoVideoDevice)?;
        let video_device: ID3D11VideoDevice =
            device.cast().map_err(|_| HwDecError::NoVideoDevice)?;
        let video_context: ID3D11VideoContext =
            context.cast().map_err(|_| HwDecError::NoVideoDevice)?;
        Ok(DecodeDevice { device, context, video_device, video_context })
    }

    /// Does this driver expose HEVC Main VLD with an NV12 output? Asked of `ID3D11VideoDevice`
    /// directly and never of ffmpeg: Stage 0 recorded a build whose `-hwaccel d3d11va` enumerated
    /// ZERO GUIDs on this very box and then silently decoded in software.
    pub fn supports_hevc_main_nv12(&self) -> bool {
        // SAFETY: the profile GUID is a constant and the format is a plain enum.
        unsafe {
            self.video_device
                .CheckVideoDecoderFormat(&D3D11_DECODER_PROFILE_HEVC_VLD_MAIN, DXGI_FORMAT_NV12)
                .map(|b| b.as_bool())
                .unwrap_or(false)
        }
    }

    /// Every decoder profile GUID the driver advertises — for the report, not for a decision.
    pub fn profile_count(&self) -> u32 {
        // SAFETY: no arguments, no out-parameters.
        unsafe { self.video_device.GetVideoDecoderProfileCount() }
    }

    fn removed(&self) -> bool {
        // SAFETY: no arguments; the call reports the device's own state.
        unsafe { self.device.GetDeviceRemovedReason().is_err() }
    }
}

/// A live decoder plus its surfaces, sized for one tile geometry.
///
/// All tiles of a photo share one of these — the plan's non-negotiable #1 (one pipelined session
/// per photo) begins here, and `decode_tile` is deliberately re-entrant on the same decoder so the
/// session-reuse timing row measures the real product shape.
pub struct DecodeSession {
    dev: DecodeDevice,
    decoder: ID3D11VideoDecoder,
    config: D3D11_VIDEO_DECODER_CONFIG,
    /// The NV12 texture ARRAY the driver decodes into, and one output view per slice.
    _surfaces: ID3D11Texture2D,
    views: Vec<ID3D11VideoDecoderOutputView>,
    in_flight: Vec<bool>,
    next_surface: usize,
    /// A single-slice staging texture used to read a decoded surface back to system memory.
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    status_report: u32,
    poisoned: Option<HwDecError>,
    /// `(submit_ms, readback_ms)` of the most recent picture — see [`DecodeSession::last_split_ms`].
    last_split: (f64, f64),
}

impl DecodeSession {
    /// Create a decoder for `width × height` NV12 HEVC Main, with `surfaces` output slices.
    pub fn new(width: u32, height: u32, surfaces: u32) -> Result<Self, HwDecError> {
        let dev = DecodeDevice::new()?;
        Self::on_device(dev, width, height, surfaces)
    }

    /// As [`Self::new`], on a device the caller already made.
    pub fn on_device(
        dev: DecodeDevice,
        width: u32,
        height: u32,
        surfaces: u32,
    ) -> Result<Self, HwDecError> {
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(HwDecError::Unsupported("decode dimensions out of range"));
        }
        let surfaces = surfaces.clamp(1, 64);
        if !dev.supports_hevc_main_nv12() {
            return Err(HwDecError::NoHevcProfile);
        }
        let desc = D3D11_VIDEO_DECODER_DESC {
            Guid: D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
            SampleWidth: width,
            SampleHeight: height,
            OutputFormat: DXGI_FORMAT_NV12,
        };
        // SAFETY: `desc` is a live local for the duration of both calls.
        let n = unsafe { dev.video_device.GetVideoDecoderConfigCount(&desc) }
            .map_err(|e| api("GetVideoDecoderConfigCount", e))?;
        if n == 0 {
            return Err(HwDecError::NoHevcProfile);
        }
        // Prefer a RAW-bitstream config: that is the one whose bitstream buffer takes start-code
        // prefixed NALs and whose slice control is `DXVA_Slice_HEVC_Short`, which is what
        // `crate::dxva` builds. Stage 0 measured ConfigBitstreamRaw=1 on both configs this driver
        // offers at every tile size; picking rather than assuming keeps that from being load-bearing.
        let mut config = D3D11_VIDEO_DECODER_CONFIG::default();
        let mut found = false;
        for i in 0..n {
            let mut c = D3D11_VIDEO_DECODER_CONFIG::default();
            // SAFETY: `c` is a live local out-parameter.
            if unsafe { dev.video_device.GetVideoDecoderConfig(&desc, i, &mut c) }.is_ok()
                && c.ConfigBitstreamRaw == 1
            {
                config = c;
                found = true;
                break;
            }
        }
        if !found {
            return Err(HwDecError::Unsupported("no ConfigBitstreamRaw HEVC config"));
        }
        // SAFETY: both descriptors are live locals; the interface is returned owned.
        let decoder = unsafe { dev.video_device.CreateVideoDecoder(&desc, &config) }
            .map_err(|e| api("CreateVideoDecoder", e))?;

        let td = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: surfaces,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_DECODER.0 as u32,
            ..Default::default()
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: `td` is a live local; `tex` receives an owned interface.
        unsafe { dev.device.CreateTexture2D(&td, None, Some(&mut tex)) }
            .map_err(|e| api("CreateTexture2D(NV12 decoder array)", e))?;
        let tex = tex.ok_or(HwDecError::Unsupported("null decoder surface array"))?;

        let mut views = Vec::with_capacity(surfaces as usize);
        for i in 0..surfaces {
            let mut vd = D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC {
                DecodeProfile: desc.Guid,
                ViewDimension: D3D11_VDOV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            vd.Anonymous.Texture2D.ArraySlice = i;
            let mut view: Option<ID3D11VideoDecoderOutputView> = None;
            // SAFETY: `vd` is a live local; `view` receives an owned interface.
            unsafe { dev.video_device.CreateVideoDecoderOutputView(&tex, &vd, Some(&mut view)) }
                .map_err(|e| api("CreateVideoDecoderOutputView", e))?;
            views.push(view.ok_or(HwDecError::Unsupported("null decoder output view"))?);
        }

        let sd = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `sd` is a live local; `staging` receives an owned interface.
        unsafe { dev.device.CreateTexture2D(&sd, None, Some(&mut staging)) }
            .map_err(|e| api("CreateTexture2D(NV12 staging)", e))?;
        let staging = staging.ok_or(HwDecError::Unsupported("null staging texture"))?;

        Ok(DecodeSession {
            dev,
            decoder,
            config,
            _surfaces: tex,
            views,
            in_flight: vec![false; surfaces as usize],
            next_surface: 0,
            staging,
            width,
            height,
            status_report: 0,
            poisoned: None,
            last_split: (0.0, 0.0),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn surface_count(&self) -> usize {
        self.views.len()
    }
    /// The `ConfigDecoderSpecific`/`ConfigBitstreamRaw` the driver handed back — reported so a
    /// tester's row can be compared against this box's.
    pub fn config(&self) -> (u32, u16) {
        (self.config.ConfigBitstreamRaw, self.config.ConfigDecoderSpecific)
    }

    /// Take the next free surface slice, round-robin. `None` when every slice is still in flight,
    /// which with the synchronous readback below can only happen if a caller leaked a lease.
    fn acquire(&mut self) -> Option<SurfaceLease> {
        let n = self.views.len();
        for k in 0..n {
            let i = (self.next_surface + k) % n;
            if !self.in_flight[i] {
                self.in_flight[i] = true;
                self.next_surface = (i + 1) % n;
                return Some(SurfaceLease { index: i as u8 });
            }
        }
        None
    }

    fn release(&mut self, lease: SurfaceLease) {
        if let Some(f) = self.in_flight.get_mut(lease.index as usize) {
            *f = false;
        }
    }

    /// Decode one tile's item data (E1's length-prefixed NAL stream) into NV12.
    ///
    /// `ps` is the file's parameter sets; `item` is exactly the bytes E1's extents name. The
    /// picture is assumed intra and IDR, which every corpus tile is (NAL 20, `IDR_N_LP`) — and the
    /// assumption is CHECKED against the NAL types rather than trusted.
    pub fn decode_tile(
        &mut self,
        ps: &ParameterSets,
        item: &[u8],
    ) -> Result<Nv12Image, HwDecError> {
        self.decode_tile_impl(ps, item, QmatrixPolicy::FromParameterSets)
    }

    /// As [`Self::decode_tile`], with the quantisation-matrix buffer under explicit control. Only a
    /// falsifier ever passes anything but `QmatrixPolicy::FromParameterSets` — which is now
    /// structural rather than aspirational: v0.8.152 (5.1(b)) puts this door behind the
    /// `falsifiers` feature, so it exists for test and example targets and does not exist in the
    /// library `falcon-native` links.
    #[cfg(feature = "falsifiers")]
    #[doc(hidden)]
    pub fn decode_tile_with(
        &mut self,
        ps: &ParameterSets,
        item: &[u8],
        qpolicy: QmatrixPolicy,
    ) -> Result<Nv12Image, HwDecError> {
        self.decode_tile_impl(ps, item, qpolicy)
    }

    fn decode_tile_impl(
        &mut self,
        ps: &ParameterSets,
        item: &[u8],
        qpolicy: QmatrixPolicy,
    ) -> Result<Nv12Image, HwDecError> {
        let lease = self.submit_tile(ps, item, qpolicy)?;
        let out = self.finish(lease);
        self.check_device()?;
        out
    }

    /// Decode a run of tiles PIPELINED: submit up to `surface_count` pictures before reading any of
    /// them back, then drain.
    ///
    /// This is the plan's non-negotiable #1 shape — all of a photo's tiles through one session,
    /// submission not stalled on the previous picture's readback. The synchronous
    /// [`Self::decode_tile`] serialises on a `Map` per picture, which is correct but is a GPU sync
    /// per tile; here the sync happens once per batch of `surface_count`. The surface-lifetime rule
    /// is unchanged and is what caps the batch: a slice is not reused until the readback that
    /// consumed it has completed.
    pub fn decode_tiles(
        &mut self,
        ps: &ParameterSets,
        items: &[Vec<u8>],
    ) -> Result<Vec<Nv12Image>, HwDecError> {
        let mut out = Vec::with_capacity(items.len());
        // Never superseded: this door has no caller who could have moved on. See [`TileRun`].
        self.decode_tiles_streaming(ps, items, |_, img| {
            out.push(img);
            Ok(())
        }, &mut || false)?;
        Ok(out)
    }

    /// v0.8.147 (E3-M2) — [`Self::decode_tiles`]'s body, with the collecting `Vec` replaced by a
    /// callback that sees each picture ONCE, in order, while the next few are still in flight.
    ///
    /// This is the shape the full-photo assembly needs and the reason it exists: `decode_tiles`
    /// hands back every tile of a 48 MP photo at once, which is 74 MB of NV12 resident in system
    /// memory for as long as the caller holds it. Streaming, the assembly writes each tile into its
    /// GPU canvas as it lands and the largest thing alive is one 1.4 MB tile. `decode_tiles` is now
    /// written ON this, so there is one pipelining implementation and the M1 rows that byte-pin its
    /// output are pinning this code.
    ///
    /// A callback that returns `Err` stops the run and that error is what comes back — the photo
    /// fails closed on an upload failure exactly as it does on a decode failure.
    ///
    /// The picture arrives BY VALUE. A borrow would read more naturally and would cost
    /// `decode_tiles` a 1.4 MB clone per tile to rebuild the `Vec` it promises — 74 MB of pointless
    /// copying on a 48 MP photo, measured at ~12 ms, on the very function M1's byte pins run
    /// through. The assembly borrows from the owned value and drops it, which is the same thing
    /// without the copy.
    /// v0.8.171 (HEIC SPEED PRIORITY) — `superseded` is asked ONCE PER CHUNK, at the top, and a
    /// `true` stops the run and returns [`TileRun::Aborted`]. See that type for why an abort is not
    /// an error, and this function's body for why the top of the chunk loop is the only place the
    /// question can be asked safely.
    pub fn decode_tiles_streaming<F>(
        &mut self,
        ps: &ParameterSets,
        items: &[Vec<u8>],
        mut on_picture: F,
        superseded: &mut dyn FnMut() -> bool,
    ) -> Result<TileRun, HwDecError>
    where
        F: FnMut(usize, Nv12Image) -> Result<(), HwDecError>,
    {
        let mut index = 0usize;
        for chunk in items.chunks(self.views.len()) {
            // ── v0.8.171: THE ABORT POINT, and there is exactly one ────────────────────────────
            //
            // HERE and nowhere else, because here NOTHING is live. The previous chunk was fully
            // drained (every lease finished or released below), `check_device` has run, and no
            // `DecoderBeginFrame` is open, no staging buffer is mapped, no decoder buffer is checked
            // out — `decode_into` opens and closes a frame inside one `submit_tile`, and `readback`
            // maps and unmaps inside one `finish`. Returning from this line leaks nothing at all,
            // which is a property of the loop's shape rather than a promise the caller has to keep.
            //
            // ASKED PER CHUNK, NOT PER TILE, and that is the right grain for both halves of the
            // trade: a chunk is `views.len()` (8) pictures, so a ~190-tile 48 MP photo asks ~24
            // times — often enough that the worst case is ~4 % of the photo's decode rather than all
            // of it, and rare enough that the question itself is free. Per TILE would mean asking
            // inside the submit loop, where up to 8 leases are outstanding: legal (the release loop
            // below already handles exactly that shape) but it would abort a picture whose surfaces
            // are mid-flight on the engine, which buys nothing the next chunk boundary does not.
            if superseded() {
                return Ok(TileRun::Aborted { done: index, total: items.len() });
            }
            let mut leases = Vec::with_capacity(chunk.len());
            for item in chunk {
                match self.submit_tile(ps, item, QmatrixPolicy::FromParameterSets) {
                    Ok(l) => leases.push(l),
                    Err(e) => {
                        for l in leases {
                            self.release(l);
                        }
                        return Err(e);
                    }
                }
            }
            // v0.8.147: every lease in this chunk is RELEASED even when the run stops early. The
            // v0.8.146 loop returned out of the middle of `for l in leases`, dropping the
            // not-yet-finished leases with `in_flight[i]` still true — after which those slices
            // were gone for the session's life and a long-enough run afterwards met
            // `SurfacesExhausted` with nothing to say why. It could only fire on a readback
            // failure, which is why no M1 row caught it; the photo path's "a bad tile leaves the
            // session usable" gate is what does, because there the callback can fail too.
            let mut result = Ok(());
            let mut rest = leases.into_iter();
            for l in rest.by_ref() {
                match self.finish(l) {
                    Ok(img) => {
                        if let Err(e) = on_picture(index, img) {
                            result = Err(e);
                            break;
                        }
                        index += 1;
                    }
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
            for l in rest {
                self.release(l);
            }
            result?;
            self.check_device()?;
        }
        Ok(TileRun::Complete)
    }

    /// How long the LAST decode spent submitting versus reading back, in milliseconds.
    ///
    /// `DecoderEndFrame` is asynchronous — it queues the picture and returns — so the submit half is
    /// the CPU-side marshalling cost and the readback half is where the GPU's actual decode lands.
    /// Reported split rather than summed because E3 proper never reads back to system memory at all.
    pub fn last_split_ms(&self) -> (f64, f64) {
        self.last_split
    }

    fn check_device(&mut self) -> Result<(), HwDecError> {
        if self.dev.removed() {
            let e = HwDecError::DeviceLost;
            self.poisoned = Some(e.clone());
            return Err(e);
        }
        Ok(())
    }

    /// Validate, marshal and submit one picture. The returned lease is owed a [`Self::finish`].
    fn submit_tile(
        &mut self,
        ps: &ParameterSets,
        item: &[u8],
        qpolicy: QmatrixPolicy,
    ) -> Result<SurfaceLease, HwDecError> {
        if let Some(e) = &self.poisoned {
            return Err(e.clone());
        }
        if ps.sps.width != self.width || ps.sps.height != self.height {
            return Err(HwDecError::Unsupported("tile geometry differs from the session's"));
        }
        let nals = crate::hevc::split_item_nals(item, ps.length_size)?;
        let vcl: Vec<_> = nals.iter().copied().filter(|n| n.is_vcl()).collect();
        if vcl.is_empty() {
            return Err(HwDecError::Bitstream("tile item carries no slice NAL"));
        }
        // 19/20 are IDR_W_RADL / IDR_N_LP; 16..=23 is the IRAP range.
        let idr = vcl.iter().all(|n| n.nal_type == 19 || n.nal_type == 20);
        let irap = vcl.iter().all(|n| (16..=23).contains(&n.nal_type));
        if !irap {
            return Err(HwDecError::Unsupported("tile slice is not an IRAP picture"));
        }

        let lease = self.acquire().ok_or(HwDecError::SurfacesExhausted)?;
        let t = std::time::Instant::now();
        match self.decode_into(ps, item, &vcl, lease, qpolicy, irap, idr) {
            Ok(()) => {
                self.last_split.0 = t.elapsed().as_secs_f64() * 1e3;
                Ok(lease)
            }
            Err(e) => {
                self.release(lease);
                Err(e)
            }
        }
    }

    /// Read a submitted picture back and return its surface to the pool.
    fn finish(&mut self, lease: SurfaceLease) -> Result<Nv12Image, HwDecError> {
        let t = std::time::Instant::now();
        let out = self.readback(lease);
        self.last_split.1 = t.elapsed().as_secs_f64() * 1e3;
        self.release(lease);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_into(
        &mut self,
        ps: &ParameterSets,
        item: &[u8],
        vcl: &[crate::hevc::ItemNal],
        lease: SurfaceLease,
        qpolicy: QmatrixPolicy,
        irap: bool,
        idr: bool,
    ) -> Result<(), HwDecError> {
        self.status_report = self.status_report.wrapping_add(1).max(1);
        let pp = dxva::fill_pic_params(ps, lease.index, self.status_report, irap, idr);
        let qm = match qpolicy {
            QmatrixPolicy::FromParameterSets => {
                Some(dxva::fill_qmatrix(ps.effective_scaling_lists()))
            }
            QmatrixPolicy::Zeroed => Some(dxva::DxvaQmatrixHevc::default()),
            QmatrixPolicy::Omitted => None,
        };

        let view = self.views[lease.index as usize].clone();
        // ffmpeg retries E_PENDING here; so do we, bounded, because an unbounded retry on a wedged
        // driver is a hang and a hang is worse than a decline.
        let mut began = false;
        let mut last: windows::core::HRESULT = windows::core::HRESULT(0);
        for attempt in 0..64 {
            // SAFETY: decoder and view are live owned interfaces; no content key is used.
            match unsafe { self.dev.video_context.DecoderBeginFrame(&self.decoder, &view, 0, None) }
            {
                Ok(()) => {
                    began = true;
                    break;
                }
                Err(e) => {
                    last = e.code();
                    // E_PENDING = 0x8000000A
                    if last.0 as u32 != 0x8000_000A {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(if attempt < 8 {
                        0
                    } else {
                        2
                    }));
                }
            }
        }
        if !began {
            return Err(HwDecError::Api { call: "DecoderBeginFrame", hr: last.0 });
        }

        let submitted = self.submit_buffers(&pp, qm.as_ref(), item, vcl);
        // EndFrame is owed to the driver whatever happened between Begin and here.
        // SAFETY: the decoder is a live owned interface and a frame is open on it.
        let ended = unsafe { self.dev.video_context.DecoderEndFrame(&self.decoder) };
        submitted?;
        ended.map_err(|e| api("DecoderEndFrame", e))?;
        Ok(())
    }

    fn submit_buffers(
        &self,
        pp: &dxva::DxvaPicParamsHevc,
        qm: Option<&dxva::DxvaQmatrixHevc>,
        item: &[u8],
        vcl: &[crate::hevc::ItemNal],
    ) -> Result<(), HwDecError> {
        let mut descs: Vec<D3D11_VIDEO_DECODER_BUFFER_DESC> = Vec::with_capacity(4);

        self.write_buffer(
            D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
            dxva::as_bytes(pp),
            0,
            &mut descs,
        )?;
        if let Some(qm) = qm {
            self.write_buffer(
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                dxva::as_bytes(qm),
                0,
                &mut descs,
            )?;
        }

        // The bitstream buffer's capacity is the driver's to choose, so it is asked FIRST and the
        // picture is assembled to fit rather than assembled and hoped for.
        let mut size = 0u32;
        let mut ptr: *mut core::ffi::c_void = core::ptr::null_mut();
        // SAFETY: both out-parameters are live locals; the pointer is valid until ReleaseDecoderBuffer.
        unsafe {
            self.dev.video_context.GetDecoderBuffer(
                &self.decoder,
                D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
                &mut size,
                &mut ptr,
            )
        }
        .map_err(|e| api("GetDecoderBuffer(bitstream)", e))?;
        let assembled = dxva::build_bitstream(item, vcl, size as usize);
        let (bs, slices) = match assembled {
            Some(v) => v,
            None => {
                // SAFETY: a buffer of this type is checked out.
                let _ = unsafe {
                    self.dev
                        .video_context
                        .ReleaseDecoderBuffer(&self.decoder, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM)
                };
                return Err(HwDecError::BitstreamTooLarge {
                    need: vcl.iter().map(|n| n.len + 3).sum::<usize>(),
                    have: size as usize,
                });
            }
        };
        // v0.8.152 (R3-L4): a null `ptr` from a succeeding `GetDecoderBuffer` FAILS CLOSED, the way
        // `write_buffer` folds the identical condition into its `fits` test twenty lines below.
        // Until now the copy was skipped and the descriptor was pushed anyway with
        // `DataSize: bs.len()`, so `SubmitDecoderBuffers` handed the driver an UNINITIALISED
        // bitstream buffer to decode: a wrong picture, no error, and no fall-soft to WIC. The
        // release still happens either way — it is owed to the driver whatever we then report.
        let bitstream_ptr_null = ptr.is_null();
        if !bitstream_ptr_null {
            // SAFETY: the driver's buffer is at least `size` bytes and `bs.len() <= size` by
            // construction in `build_bitstream`; the regions cannot overlap (one is driver memory).
            unsafe { core::ptr::copy_nonoverlapping(bs.as_ptr(), ptr.cast::<u8>(), bs.len()) };
        }
        // SAFETY: a buffer of this type is checked out.
        unsafe {
            self.dev
                .video_context
                .ReleaseDecoderBuffer(&self.decoder, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM)
        }
        .map_err(|e| api("ReleaseDecoderBuffer(bitstream)", e))?;
        if bitstream_ptr_null {
            return Err(HwDecError::Api {
                call: "GetDecoderBuffer(bitstream) returned a null pointer",
                hr: 0,
            });
        }
        descs.push(D3D11_VIDEO_DECODER_BUFFER_DESC {
            BufferType: D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
            DataSize: bs.len() as u32,
            ..Default::default()
        });

        // The slice-control array LAST, because its offsets describe the bitstream just written.
        let mut sc: Vec<u8> = Vec::with_capacity(slices.len() * core::mem::size_of::<dxva::DxvaSliceHevcShort>());
        for s in &slices {
            sc.extend_from_slice(dxva::as_bytes(s));
        }
        self.write_buffer(
            D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
            &sc,
            slices.len() as u32,
            &mut descs,
        )?;

        // SAFETY: every desc names a buffer written and released above.
        unsafe { self.dev.video_context.SubmitDecoderBuffers(&self.decoder, &descs) }
            .map_err(|e| api("SubmitDecoderBuffers", e))
    }

    /// Check out a driver buffer, copy `payload` into it, release it, and record its descriptor.
    fn write_buffer(
        &self,
        kind: D3D11_VIDEO_DECODER_BUFFER_TYPE,
        payload: &[u8],
        mbs: u32,
        descs: &mut Vec<D3D11_VIDEO_DECODER_BUFFER_DESC>,
    ) -> Result<(), HwDecError> {
        let mut size = 0u32;
        let mut ptr: *mut core::ffi::c_void = core::ptr::null_mut();
        // SAFETY: both out-parameters are live locals.
        unsafe {
            self.dev.video_context.GetDecoderBuffer(&self.decoder, kind, &mut size, &mut ptr)
        }
        .map_err(|e| api("GetDecoderBuffer", e))?;
        let fits = payload.len() <= size as usize && !ptr.is_null();
        if fits {
            // SAFETY: the driver's buffer is `size` bytes and `payload.len() <= size`.
            unsafe { core::ptr::copy_nonoverlapping(payload.as_ptr(), ptr.cast::<u8>(), payload.len()) };
        }
        // SAFETY: a buffer of this type is checked out; it must be released either way.
        unsafe { self.dev.video_context.ReleaseDecoderBuffer(&self.decoder, kind) }
            .map_err(|e| api("ReleaseDecoderBuffer", e))?;
        if !fits {
            return Err(HwDecError::BufferTooSmall { need: payload.len(), have: size as usize });
        }
        descs.push(D3D11_VIDEO_DECODER_BUFFER_DESC {
            BufferType: kind,
            DataSize: payload.len() as u32,
            NumMBsInBuffer: mbs,
            ..Default::default()
        });
        Ok(())
    }

    /// Copy the decoded slice into the staging texture and map it. The map is the sync point that
    /// makes the surface reusable — see the module header.
    fn readback(&mut self, lease: SurfaceLease) -> Result<Nv12Image, HwDecError> {
        let src: ID3D11Resource = self._surfaces.cast().map_err(|e| api("cast decoder array", e))?;
        let dst: ID3D11Resource = self.staging.cast().map_err(|e| api("cast staging", e))?;
        // SAFETY: both resources are live, same format and dimensions; the subresource index of
        // array slice `i` with one mip level is `i`.
        unsafe {
            self.dev.context.CopySubresourceRegion(
                &dst,
                0,
                0,
                0,
                0,
                &src,
                lease.index as u32,
                None,
            )
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `mapped` is a live local; the staging texture was made CPU-readable.
        unsafe { self.dev.context.Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| api("Map(staging NV12)", e))?;
        let stride = mapped.RowPitch as usize;
        let rows = self.height as usize + (self.height as usize).div_ceil(2);
        let mut data = vec![0u8; stride * rows];
        // v0.8.152 (R3-L5): the copy's bound is now the DRIVER'S answer as well as our arithmetic.
        // `D3D11_MAPPED_SUBRESOURCE` carries `DepthPitch` — the mapped extent — beside `RowPitch`,
        // and only `RowPitch` was ever read: the `copy_nonoverlapping` below took exactly
        // `stride × (H + ceil(H/2))` bytes out of driver memory on the strength of the documented
        // NV12 staging layout alone. That layout is real, so this is a hardening gap rather than a
        // known over-read — but it is a raw memcpy out of another process's mapping, and the API
        // states its own extent, so ask it.
        //
        // v0.8.153 (skeptic A / O2) — THE BOUND STOPS REFUSING CONFORMANT DRIVERS. `DepthPitch` is
        // specified as the distance between DEPTH SLICES, and a 2D texture has exactly one; for a
        // PLANAR format there is no single right answer and drivers give three different ones, all
        // legal: `0` (not applicable — nothing to report), `RowPitch × Height` (the LUMA slice
        // extent, which is 2/3 of the NV12 mapping because the chroma plane is a further
        // `ceil(H/2)` rows), or the full `RowPitch × (H + ceil(H/2))`. v0.8.152's `< data.len()`
        // declined the first two, so on a driver that reports the luma extent — the shape this crate
        // has NOT measured, its evidence base being one vendor — the whole hardware lane would have
        // died at boot on a value that was never wrong.
        //
        // So decline only what is UNAMBIGUOUSLY wrong: a reported, non-zero extent that does not
        // even cover the luma plane. A driver that says that has contradicted `RowPitch` and
        // `Height`, which are the two numbers this readback is built on. Everything at or above the
        // luma extent is treated as unreported-but-consistent, and the sub-`data.len()` case gets ONE
        // line per session so a field report can still name it.
        let luma_extent = stride * self.height as usize;
        if mapped.DepthPitch != 0 && (mapped.DepthPitch as usize) < luma_extent {
            // SAFETY: the same subresource was mapped immediately above; it must be unmapped
            // before this function returns, on this path as on every other.
            unsafe { self.dev.context.Unmap(&dst, 0) };
            return Err(HwDecError::Api {
                call: "Map(staging NV12) reported a DepthPitch smaller than the luma plane",
                hr: 0,
            });
        }
        if (mapped.DepthPitch as usize) < data.len() {
            // The luma-slice reporter (or a `0`). Not a defect — but it is the one place a future
            // "the photos come back wrong on vendor X" report would start, so say it ONCE.
            note_once(format!(
                "staging Map reports DepthPitch={} for an NV12 mapping of {} bytes (RowPitch={}, \
                 H={}) — read as the luma-slice extent, which is legal for a planar format; the \
                 copy stays bounded by our own layout",
                mapped.DepthPitch,
                data.len(),
                stride,
                self.height
            ));
        }
        if !mapped.pData.is_null() {
            // SAFETY: a mapped NV12 staging texture exposes `RowPitch × (H + ceil(H/2))` bytes —
            // the luma plane followed by the interleaved chroma plane at the same pitch.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    mapped.pData.cast::<u8>(),
                    data.as_mut_ptr(),
                    data.len(),
                )
            };
        }
        // SAFETY: the same subresource was mapped immediately above.
        unsafe { self.dev.context.Unmap(&dst, 0) };
        if mapped.pData.is_null() {
            return Err(HwDecError::Api { call: "Map(staging NV12) returned null", hr: 0 });
        }
        Ok(Nv12Image { width: self.width, height: self.height, stride: stride as u32, data })
    }
}
