//! falcon-nvjpeg — optional GPU JPEG decode via NVIDIA nvJPEG (CUDA) using the
//! dedicated hardware JPEG unit with on-decode downscaling. A 45 MP Canon JPG
//! decodes in ~35 ms vs ~236 ms on the multithreaded CPU decoder; a 1/2 scale lands
//! the Reference path near the 4096 px target with no extra resize (PLAN §17).
//!
//! The CUDA DLLs are dlopen'd at runtime via `libloading`, so `falcon-app` BUILDS
//! and RUNS on machines without CUDA — [`NvjpegContext::new`] returns `None` there
//! and the caller falls back to the pure-Rust decoder. There is no link-time
//! dependency on the CUDA toolkit.
//!
//! Constraint: `nvjpegDecodeParamsSetScaleFactor` works only with the HARDWARE
//! backend, which decodes baseline single-scan JPEGs (Canon in-camera JPGs
//! qualify). Every decode is gated by `nvjpegDecoderJpegSupported`; an unsupported
//! bitstream (progressive, odd subsampling) yields `None` so the caller uses the
//! CPU path. Output is interleaved RGB8 (`channel[0]`), tight-packed `w*h*3`.

use std::os::raw::{c_int, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use falcon_decode::{DecodeCaps, DecodeError, DecodedPixels, ImageDecoder, Shot};
use libloading::Library;

// nvjpegOutputFormat_t::NVJPEG_OUTPUT_RGBI — interleaved RGB in channel[0].
const NVJPEG_OUTPUT_RGBI: c_int = 5;
// nvjpegBackend_t::NVJPEG_BACKEND_HARDWARE — the dedicated NVJPG unit (does scaling).
const NVJPEG_BACKEND_HARDWARE: c_int = 3;
// cudaMemcpyKind::cudaMemcpyDeviceToHost.
const CUDA_MEMCPY_DEVICE_TO_HOST: c_int = 2;

// Opaque nvJPEG / CUDA handles. All are `*mut c_void`; the distinct aliases just
// document which handle each FFI slot expects.
type NvHandle = *mut c_void;
type NvDecoder = *mut c_void;
type NvState = *mut c_void;
type NvPinned = *mut c_void;
type NvDevice = *mut c_void;
type NvStream = *mut c_void;
type NvParams = *mut c_void;
type CudaStream = *mut c_void;

/// nvjpegImage_t — up to 4 channel planes + pitches. RGBI writes channel[0] only.
#[repr(C)]
struct NvjpegImage {
    channel: [*mut u8; 4],
    pitch: [usize; 4],
}

// Hand-declared FFI signatures (no bindgen — clang is absent). Status returns are
// `nvjpegStatus_t` / `cudaError_t`; 0 == success.
type FnCreateEx =
    unsafe extern "C" fn(c_int, *mut c_void, *mut c_void, c_uint, *mut NvHandle) -> c_int;
type FnDestroy = unsafe extern "C" fn(NvHandle) -> c_int;
type FnDecoderCreate = unsafe extern "C" fn(NvHandle, c_int, *mut NvDecoder) -> c_int;
type FnDecoderStateCreate = unsafe extern "C" fn(NvHandle, NvDecoder, *mut NvState) -> c_int;
type FnBufferPinnedCreate = unsafe extern "C" fn(NvHandle, *mut c_void, *mut NvPinned) -> c_int;
type FnBufferDeviceCreate = unsafe extern "C" fn(NvHandle, *mut c_void, *mut NvDevice) -> c_int;
type FnStateAttachPinned = unsafe extern "C" fn(NvState, NvPinned) -> c_int;
type FnStateAttachDevice = unsafe extern "C" fn(NvState, NvDevice) -> c_int;
type FnJpegStreamCreate = unsafe extern "C" fn(NvHandle, *mut NvStream) -> c_int;
type FnJpegStreamParse =
    unsafe extern "C" fn(NvHandle, *const u8, usize, c_int, c_int, NvStream) -> c_int;
type FnGetFrameDimensions = unsafe extern "C" fn(NvStream, *mut c_uint, *mut c_uint) -> c_int;
type FnDecodeParamsCreate = unsafe extern "C" fn(NvHandle, *mut NvParams) -> c_int;
type FnSetOutputFormat = unsafe extern "C" fn(NvParams, c_int) -> c_int;
type FnSetScaleFactor = unsafe extern "C" fn(NvParams, c_int) -> c_int;
type FnDecoderJpegSupported =
    unsafe extern "C" fn(NvDecoder, NvStream, NvParams, *mut c_int) -> c_int;
type FnDecodeJpeg = unsafe extern "C" fn(
    NvHandle,
    NvDecoder,
    NvState,
    NvStream,
    *mut NvjpegImage,
    NvParams,
    CudaStream,
) -> c_int;
// v0.8.152 (R3-L3): the six destroys that pair with the six creates above. Every one of them was
// missing from the table, which is why nothing could destroy the objects even in principle.
type FnDecoderDestroy = unsafe extern "C" fn(NvDecoder) -> c_int;
type FnJpegStateDestroy = unsafe extern "C" fn(NvState) -> c_int;
type FnBufferPinnedDestroy = unsafe extern "C" fn(NvPinned) -> c_int;
type FnBufferDeviceDestroy = unsafe extern "C" fn(NvDevice) -> c_int;
type FnJpegStreamDestroy = unsafe extern "C" fn(NvStream) -> c_int;
type FnDecodeParamsDestroy = unsafe extern "C" fn(NvParams) -> c_int;
type FnCudaMalloc = unsafe extern "C" fn(*mut *mut c_void, usize) -> c_int;
type FnCudaFree = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnCudaMemcpy = unsafe extern "C" fn(*mut c_void, *const c_void, usize, c_int) -> c_int;
type FnCudaStreamSynchronize = unsafe extern "C" fn(CudaStream) -> c_int;

/// Resolved entry points from `nvjpeg64_13.dll` + `cudart64_13.dll`. The two
/// `Library` handles are kept so the modules stay mapped for the table's lifetime.
#[allow(dead_code)]
struct Api {
    cudart: Library,
    nvjpeg: Library,
    create_ex: FnCreateEx,
    destroy: FnDestroy,
    decoder_create: FnDecoderCreate,
    decoder_state_create: FnDecoderStateCreate,
    buffer_pinned_create: FnBufferPinnedCreate,
    buffer_device_create: FnBufferDeviceCreate,
    state_attach_pinned: FnStateAttachPinned,
    state_attach_device: FnStateAttachDevice,
    jpeg_stream_create: FnJpegStreamCreate,
    jpeg_stream_parse: FnJpegStreamParse,
    get_frame_dimensions: FnGetFrameDimensions,
    decode_params_create: FnDecodeParamsCreate,
    set_output_format: FnSetOutputFormat,
    set_scale_factor: FnSetScaleFactor,
    decoder_jpeg_supported: FnDecoderJpegSupported,
    decode_jpeg: FnDecodeJpeg,
    decoder_destroy: FnDecoderDestroy,
    jpeg_state_destroy: FnJpegStateDestroy,
    buffer_pinned_destroy: FnBufferPinnedDestroy,
    buffer_device_destroy: FnBufferDeviceDestroy,
    jpeg_stream_destroy: FnJpegStreamDestroy,
    decode_params_destroy: FnDecodeParamsDestroy,
    cuda_malloc: FnCudaMalloc,
    cuda_free: FnCudaFree,
    cuda_memcpy: FnCudaMemcpy,
    cuda_stream_synchronize: FnCudaStreamSynchronize,
}

/// Load a CUDA DLL: try the OS search path (CUDA `bin\x64` on PATH) first, then
/// the toolkit's `bin\x64` directly via `CUDA_PATH` so it works even when PATH was
/// never augmented. Returns `None` if absent (no CUDA → CPU fallback).
fn load_lib(name: &str) -> Option<Library> {
    unsafe {
        if let Ok(l) = Library::new(name) {
            return Some(l);
        }
        if let Ok(cuda) = std::env::var("CUDA_PATH") {
            if let Ok(l) = Library::new(format!("{cuda}\\bin\\x64\\{name}")) {
                return Some(l);
            }
        }
    }
    None
}

impl Api {
    /// dlopen both DLLs and resolve every symbol. `None` if a DLL or symbol is
    /// missing. Each `*lib.get(..).ok()?` ends its borrow at the statement's `;`,
    /// so the `Library` values are free to move into the struct afterwards.
    unsafe fn load() -> Option<Self> {
        // cudart first so it's resident when nvjpeg resolves its dependency on it.
        let cudart = load_lib("cudart64_13.dll")?;
        let nvjpeg = load_lib("nvjpeg64_13.dll")?;

        let create_ex: FnCreateEx = *nvjpeg.get(b"nvjpegCreateEx\0").ok()?;
        let destroy: FnDestroy = *nvjpeg.get(b"nvjpegDestroy\0").ok()?;
        let decoder_create: FnDecoderCreate = *nvjpeg.get(b"nvjpegDecoderCreate\0").ok()?;
        let decoder_state_create: FnDecoderStateCreate =
            *nvjpeg.get(b"nvjpegDecoderStateCreate\0").ok()?;
        let buffer_pinned_create: FnBufferPinnedCreate =
            *nvjpeg.get(b"nvjpegBufferPinnedCreate\0").ok()?;
        let buffer_device_create: FnBufferDeviceCreate =
            *nvjpeg.get(b"nvjpegBufferDeviceCreate\0").ok()?;
        let state_attach_pinned: FnStateAttachPinned =
            *nvjpeg.get(b"nvjpegStateAttachPinnedBuffer\0").ok()?;
        let state_attach_device: FnStateAttachDevice =
            *nvjpeg.get(b"nvjpegStateAttachDeviceBuffer\0").ok()?;
        let jpeg_stream_create: FnJpegStreamCreate = *nvjpeg.get(b"nvjpegJpegStreamCreate\0").ok()?;
        let jpeg_stream_parse: FnJpegStreamParse = *nvjpeg.get(b"nvjpegJpegStreamParse\0").ok()?;
        let get_frame_dimensions: FnGetFrameDimensions =
            *nvjpeg.get(b"nvjpegJpegStreamGetFrameDimensions\0").ok()?;
        let decode_params_create: FnDecodeParamsCreate =
            *nvjpeg.get(b"nvjpegDecodeParamsCreate\0").ok()?;
        let set_output_format: FnSetOutputFormat =
            *nvjpeg.get(b"nvjpegDecodeParamsSetOutputFormat\0").ok()?;
        let set_scale_factor: FnSetScaleFactor =
            *nvjpeg.get(b"nvjpegDecodeParamsSetScaleFactor\0").ok()?;
        let decoder_jpeg_supported: FnDecoderJpegSupported =
            *nvjpeg.get(b"nvjpegDecoderJpegSupported\0").ok()?;
        let decode_jpeg: FnDecodeJpeg = *nvjpeg.get(b"nvjpegDecodeJpeg\0").ok()?;
        // v0.8.152 (R3-L3). Resolved with `.ok()?` like every other symbol: each of these ships in
        // the same nvJPEG API generation as the `…Create` above it, so a DLL that has the creates
        // and not the destroys is not a configuration that exists — and if it somehow did, the
        // honest answer is the CPU fallback rather than a context that cannot clean up after itself.
        let decoder_destroy: FnDecoderDestroy = *nvjpeg.get(b"nvjpegDecoderDestroy\0").ok()?;
        let jpeg_state_destroy: FnJpegStateDestroy = *nvjpeg.get(b"nvjpegJpegStateDestroy\0").ok()?;
        let buffer_pinned_destroy: FnBufferPinnedDestroy =
            *nvjpeg.get(b"nvjpegBufferPinnedDestroy\0").ok()?;
        let buffer_device_destroy: FnBufferDeviceDestroy =
            *nvjpeg.get(b"nvjpegBufferDeviceDestroy\0").ok()?;
        let jpeg_stream_destroy: FnJpegStreamDestroy =
            *nvjpeg.get(b"nvjpegJpegStreamDestroy\0").ok()?;
        let decode_params_destroy: FnDecodeParamsDestroy =
            *nvjpeg.get(b"nvjpegDecodeParamsDestroy\0").ok()?;

        let cuda_malloc: FnCudaMalloc = *cudart.get(b"cudaMalloc\0").ok()?;
        let cuda_free: FnCudaFree = *cudart.get(b"cudaFree\0").ok()?;
        let cuda_memcpy: FnCudaMemcpy = *cudart.get(b"cudaMemcpy\0").ok()?;
        let cuda_stream_synchronize: FnCudaStreamSynchronize =
            *cudart.get(b"cudaStreamSynchronize\0").ok()?;

        Some(Api {
            cudart,
            nvjpeg,
            create_ex,
            destroy,
            decoder_create,
            decoder_state_create,
            buffer_pinned_create,
            buffer_device_create,
            state_attach_pinned,
            state_attach_device,
            jpeg_stream_create,
            jpeg_stream_parse,
            get_frame_dimensions,
            decode_params_create,
            set_output_format,
            set_scale_factor,
            decoder_jpeg_supported,
            decode_jpeg,
            decoder_destroy,
            jpeg_state_destroy,
            buffer_pinned_destroy,
            buffer_device_destroy,
            jpeg_stream_destroy,
            decode_params_destroy,
            cuda_malloc,
            cuda_free,
            cuda_memcpy,
            cuda_stream_synchronize,
        })
    }
}

/// A persistent hardware-backend nvJPEG decode context: handle, decoder, state,
/// attached pinned/device staging buffers, a reusable jpeg-stream parser, decode
/// params (output = RGBI), and one growable device output buffer reused across
/// decodes. Build once with [`new`](Self::new); call [`decode_scaled`] per image.
pub struct NvjpegContext {
    api: Api,
    handle: NvHandle,
    decoder: NvDecoder,
    state: NvState,
    /// v0.8.152 (R3-L3): the two staging buffers are now HELD. They used to be dropped on the floor
    /// the instant `state_attach_*` returned, so they could not have been destroyed even by a `Drop`
    /// that wanted to — the handles were simply gone.
    pinned: NvPinned,
    device: NvDevice,
    stream: NvStream,
    params: NvParams,
    /// Persistent device output buffer, grown on demand (avoids malloc/free per call).
    dev_buf: *mut c_void,
    dev_cap: usize,
}

/// v0.8.152 (R3-L3) — **the teardown, written ONCE and called from both places that owe it.**
///
/// `new()` creates six nvJPEG objects. Before this, `fail()` destroyed only the top-level handle and
/// `Drop` destroyed only `dev_buf` + the handle, so every setup failure leaked a decoder, a state
/// and two staging buffers, and the steady-state process leaked one of each. Bounded, honestly — the
/// app holds one context for the process's lifetime, so the leak did not GROW — but it made a
/// repeated failed `new()` cost real device memory and it foreclosed any future per-thread or
/// per-folder context.
///
/// **DESTROY ORDER: exactly the reverse of creation** — params, stream, device buffer, pinned
/// buffer, state, decoder — with the top-level handle destroyed by the caller afterwards, because
/// every one of these was created FROM that handle. The buffers go before the state they are
/// attached to, matching NVIDIA's own decoupled-decode sample; the state is destroyed immediately
/// after and never touched in between.
///
/// Each argument may be null (that is how a partially-built context is torn down) and a null is
/// skipped rather than passed to a destroy that would reject it.
///
/// # Safety
/// Every non-null handle must be a live object created from `api`'s nvJPEG handle, and none may be
/// used afterwards.
unsafe fn destroy_children(
    api: &Api,
    decoder: NvDecoder,
    state: NvState,
    pinned: NvPinned,
    device: NvDevice,
    stream: NvStream,
    params: NvParams,
) {
    if !params.is_null() {
        (api.decode_params_destroy)(params);
    }
    if !stream.is_null() {
        (api.jpeg_stream_destroy)(stream);
    }
    if !device.is_null() {
        (api.buffer_device_destroy)(device);
    }
    if !pinned.is_null() {
        (api.buffer_pinned_destroy)(pinned);
    }
    if !state.is_null() {
        (api.jpeg_state_destroy)(state);
    }
    if !decoder.is_null() {
        (api.decoder_destroy)(decoder);
    }
}

// The raw handles/pointers are only ever touched while the owner holds the context
// (the app wraps it in a `Mutex`), so it's safe to move between threads. Not `Sync`
// — concurrent decodes must serialize through the mutex.
unsafe impl Send for NvjpegContext {}

impl NvjpegContext {
    /// Initialise the hardware-backend decode context. Returns `None` when CUDA /
    /// nvJPEG is unavailable or the hardware backend can't be created (e.g. no
    /// NVJPG unit), so the caller can fall back to the CPU decoder.
    pub fn new() -> Option<Self> {
        unsafe {
            let api = Api::load()?;

            let mut handle: NvHandle = ptr::null_mut();
            if (api.create_ex)(
                NVJPEG_BACKEND_HARDWARE,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut handle,
            ) != 0
            {
                return None;
            }
            // Every child starts null and is filled in as it is created, so the one failure path
            // below can tear down whatever exists at the moment it is taken (v0.8.152, R3-L3 — it
            // used to destroy the handle alone and leak the rest, on EVERY setup failure).
            let mut decoder: NvDecoder = ptr::null_mut();
            let mut state: NvState = ptr::null_mut();
            let mut pinned: NvPinned = ptr::null_mut();
            let mut device: NvDevice = ptr::null_mut();
            let mut stream: NvStream = ptr::null_mut();
            let mut params: NvParams = ptr::null_mut();

            macro_rules! fail {
                () => {{
                    destroy_children(&api, decoder, state, pinned, device, stream, params);
                    (api.destroy)(handle);
                    return None;
                }};
            }

            if (api.decoder_create)(handle, NVJPEG_BACKEND_HARDWARE, &mut decoder) != 0 {
                fail!();
            }
            if (api.decoder_state_create)(handle, decoder, &mut state) != 0 {
                fail!();
            }
            if (api.buffer_pinned_create)(handle, ptr::null_mut(), &mut pinned) != 0 {
                fail!();
            }
            if (api.buffer_device_create)(handle, ptr::null_mut(), &mut device) != 0 {
                fail!();
            }
            if (api.state_attach_pinned)(state, pinned) != 0
                || (api.state_attach_device)(state, device) != 0
            {
                fail!();
            }
            if (api.jpeg_stream_create)(handle, &mut stream) != 0 {
                fail!();
            }
            if (api.decode_params_create)(handle, &mut params) != 0 {
                fail!();
            }
            if (api.set_output_format)(params, NVJPEG_OUTPUT_RGBI) != 0 {
                fail!();
            }

            Some(NvjpegContext {
                api,
                handle,
                decoder,
                state,
                pinned,
                device,
                stream,
                params,
                dev_buf: ptr::null_mut(),
                dev_cap: 0,
            })
        }
    }

    /// Decode `jpeg`, downscaling on the hardware unit so the long side is ≈
    /// `long_side` px. Returns tight-packed RGB8 + actual `(w, h)`, or `None` if
    /// the bitstream is unsupported by the hardware backend (caller → CPU path).
    ///
    /// The scale factor is chosen from the full frame dimensions (1/2 for a
    /// ~4096 px Reference of an 8192 px source, 1/4 for ~2048 px Fast), so the
    /// result needs little or no further CPU resize.
    pub fn decode_scaled(&mut self, jpeg: &[u8], long_side: u32) -> Option<(Vec<u8>, u32, u32)> {
        unsafe {
            // v0.7.3 GF5: defensively (re)assert OUR output format. decode_full_yuv flips the
            // SHARED params to planar YUV and restores RGBI via a closure whose
            // set_output_format return is discarded — if that restore ever failed, this fn
            // would decode planar YUV into an RGBI-sized buffer (garbage, no error). Its
            // correctness must not depend on another method's cleanup.
            if (self.api.set_output_format)(self.params, NVJPEG_OUTPUT_RGBI) != 0 {
                return None;
            }
            // Parse the bitstream + read its full dimensions.
            if (self.api.jpeg_stream_parse)(
                self.handle,
                jpeg.as_ptr(),
                jpeg.len(),
                0,
                0,
                self.stream,
            ) != 0
            {
                return None;
            }
            let (mut fw, mut fh): (c_uint, c_uint) = (0, 0);
            if (self.api.get_frame_dimensions)(self.stream, &mut fw, &mut fh) != 0
                || fw == 0
                || fh == 0
            {
                return None;
            }

            // Pick + apply the scale factor, then ask whether the hardware backend
            // can actually decode this bitstream at these params.
            let sf = scale_factor(fw.max(fh), long_side);
            if (self.api.set_scale_factor)(self.params, sf) != 0 {
                return None;
            }
            // `is_supported` is an nvjpegStatus_t: 0 == SUCCESS (supported); any
            // non-zero is a rejection reason (e.g. 7 = ARCH_MISMATCH, which the
            // software backends return for scaled decode). Fall back to CPU then.
            let mut is_supported: c_int = -1;
            if (self.api.decoder_jpeg_supported)(
                self.decoder,
                self.stream,
                self.params,
                &mut is_supported,
            ) != 0
                || is_supported != 0
            {
                return None;
            }

            // Scaled output dims: ceil(dim / factor) — matches nvJPEG's rounding
            // (verified against a CPU decode during bring-up; see probe example).
            let factor = 1u32 << sf;
            let ow = (fw + factor - 1) / factor;
            let oh = (fh + factor - 1) / factor;
            let out_len = ow as usize * oh as usize * 3;
            // The NVJPG hardware writes into `dev_buf` on an UNCHECKED FFI contract, sized here from our
            // ceil(dim/factor) assumption. The documented scaled output is exactly ow×oh, but as insurance
            // against a driver that ever emitted the uncropped MCU-aligned buffer instead (or rounded a few
            // texels larger), size the DEVICE ALLOCATION from the full frame rounded UP to the 16px MCU grid
            // before scaling — which is ≥ any plausible real nvJPEG output, in both dims, even when ow/oh
            // are already 16-aligned. The returned image + host copy stay exactly ow*oh*3 (pitch = ow*3), so
            // this is pure insurance with no behavior change. (falcon-security-backlog #2.)
            let fw_mcu = (fw + 15) / 16 * 16;
            let fh_mcu = (fh + 15) / 16 * 16;
            let alloc_len =
                ((fw_mcu + factor - 1) / factor) as usize * ((fh_mcu + factor - 1) / factor) as usize * 3;
            if !self.ensure_dev(alloc_len.max(out_len)) {
                return None;
            }

            let mut img = NvjpegImage {
                channel: [
                    self.dev_buf as *mut u8,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ],
                pitch: [ow as usize * 3, 0, 0, 0],
            };
            if (self.api.decode_jpeg)(
                self.handle,
                self.decoder,
                self.state,
                self.stream,
                &mut img,
                self.params,
                ptr::null_mut(),
            ) != 0
            {
                return None;
            }
            // v0.7.3 GF4: an async HW-decode fault surfaces ONLY at synchronize — dropping
            // the return meant memcpying the grow-only dev_buf (potentially stale pixels
            // from a PREVIOUS photo) and returning Some(). Checked like the FFI calls above.
            if (self.api.cuda_stream_synchronize)(ptr::null_mut()) != 0 {
                return None;
            }

            let mut host = vec![0u8; out_len];
            if (self.api.cuda_memcpy)(
                host.as_mut_ptr() as *mut c_void,
                self.dev_buf,
                out_len,
                CUDA_MEMCPY_DEVICE_TO_HOST,
            ) != 0
            {
                return None;
            }
            Some((host, ow, oh))
        }
    }

    /// U2 (PLAN §39.3): full-resolution decode to PLANAR YUV — the subsampled layout as stored
    /// (nvjpegOutputFormat_t NVJPEG_OUTPUT_YUV). Skips the engine's RGB colour-convert kernel and,
    /// for 4:2:2 Canon files, halves the bytes vs the RGBA the app used to upload; the fused
    /// YCbCr→RGB(+gamut) conversion runs on the GPU at upload (falcon-gpu::YuvConvert), keeping
    /// the whole chain in f32 with a SINGLE final quantisation — one fewer than the RGBI path.
    ///
    /// `max_long` > 0 rejects sources whose long side exceeds it (the caller needs a DOWNSCALED
    /// frame then — take the RGBI `decode_scaled` path; scaled YUV output is deliberately out of
    /// this fn's risk envelope). Restores the RGBI output format before returning so interleaved
    /// `decode_scaled` calls on the same context keep working.
    pub fn decode_full_yuv(&mut self, jpeg: &[u8], max_long: u32) -> Option<YuvFrame> {
        const NVJPEG_OUTPUT_YUV: c_int = 1;
        unsafe {
            let restore = |s: &Self| {
                let _ = (s.api.set_output_format)(s.params, NVJPEG_OUTPUT_RGBI);
            };
            if (self.api.set_output_format)(self.params, NVJPEG_OUTPUT_YUV) != 0 {
                return None;
            }
            if (self.api.jpeg_stream_parse)(self.handle, jpeg.as_ptr(), jpeg.len(), 0, 0, self.stream)
                != 0
            {
                restore(self);
                return None;
            }
            let (mut fw, mut fh): (c_uint, c_uint) = (0, 0);
            if (self.api.get_frame_dimensions)(self.stream, &mut fw, &mut fh) != 0 || fw == 0 || fh == 0 {
                restore(self);
                return None;
            }
            if max_long > 0 && fw.max(fh) > max_long {
                restore(self);
                return None; // needs a downscale — the caller takes the RGBI scaled path
            }
            // Chroma subsampling → per-plane divisors, read from the SOF ourselves.
            // (nvjpegJpegStreamGetChromaSubsampling proved UNRELIABLE here — it returned 444
            // then 422 for the SAME 4:2:2 file across consecutive parses, so plane sizing from
            // it was wrong. The SOF's Y-component sampling factors are the ground truth: for
            // standard files with 1×1 chroma, (dx,dy) = Y's (h,v) — 422 → (2,1), 420 → (2,2).)
            let (dx, dy): (usize, usize) = match sof_y_sampling(jpeg) {
                Some(f) => f,
                None => {
                    restore(self);
                    return None;
                }
            };
            if (self.api.set_scale_factor)(self.params, 0) != 0 {
                restore(self);
                return None;
            }
            let mut is_supported: c_int = -1;
            if (self.api.decoder_jpeg_supported)(self.decoder, self.stream, self.params, &mut is_supported)
                != 0
                || is_supported != 0
            {
                restore(self);
                return None;
            }
            // Tight plane offsets; the ALLOCATION is MCU-padded like decode_scaled's, so a
            // hypothetical driver overrun stays inside our buffer (same insurance argument).
            let (w, h) = (fw as usize, fh as usize);
            let cw = (w + dx - 1) / dx;
            let ch = (h + dy - 1) / dy;
            let (y_len, c_len) = (w * h, cw * ch);
            let total = y_len + 2 * c_len;
            let (w_mcu, h_mcu) = ((w + 15) / 16 * 16, (h + 15) / 16 * 16);
            let alloc = w_mcu * h_mcu + 2 * ((w_mcu / dx + 16) * (h_mcu / dy + 16));
            if !self.ensure_dev(alloc.max(total)) {
                restore(self);
                return None;
            }
            let base = self.dev_buf as *mut u8;
            let mut img = NvjpegImage {
                channel: [base, base.add(y_len), base.add(y_len + c_len), ptr::null_mut()],
                pitch: [w, cw, cw, 0],
            };
            if (self.api.decode_jpeg)(
                self.handle,
                self.decoder,
                self.state,
                self.stream,
                &mut img,
                self.params,
                ptr::null_mut(),
            ) != 0
            {
                restore(self);
                return None;
            }
            // v0.7.3 GF4: same as decode_scaled — a fault surfacing at synchronize means
            // dev_buf holds stale planes; restore the shared params' RGBI format on this
            // early-return too (mirrors the other `return None` sites in this fn).
            if (self.api.cuda_stream_synchronize)(ptr::null_mut()) != 0 {
                restore(self);
                return None;
            }
            // Three per-plane device→host copies from the packed buffer offsets.
            let mut y = vec![0u8; y_len];
            let mut cb = vec![0u8; c_len];
            let mut cr = vec![0u8; c_len];
            let copy = |dst: &mut [u8], off: usize| -> bool {
                (self.api.cuda_memcpy)(
                    dst.as_mut_ptr() as *mut c_void,
                    (self.dev_buf as *const u8).add(off) as *const c_void,
                    dst.len(),
                    CUDA_MEMCPY_DEVICE_TO_HOST,
                ) == 0
            };
            let ok = copy(&mut y, 0) && copy(&mut cb, y_len) && copy(&mut cr, y_len + c_len);
            restore(self);
            if !ok {
                return None;
            }
            Some(YuvFrame { y, cb, cr, w: fw, h: fh, cw: cw as u32, ch: ch as u32 })
        }
    }

    /// BENCH-ONLY wrapper (`bench-yuv`, spikes-decode-bench): total plane bytes + dims.
    #[cfg(feature = "bench-yuv")]
    pub fn decode_full_yuv_bench(&mut self, jpeg: &[u8]) -> Option<(usize, u32, u32)> {
        self.decode_full_yuv(jpeg, 0)
            .map(|f| (f.y.len() + f.cb.len() + f.cr.len(), f.w, f.h))
    }

    /// Ensure the device output buffer holds at least `need` bytes (grow-only).
    unsafe fn ensure_dev(&mut self, need: usize) -> bool {
        if need <= self.dev_cap && !self.dev_buf.is_null() {
            return true;
        }
        if !self.dev_buf.is_null() {
            (self.api.cuda_free)(self.dev_buf);
            self.dev_buf = ptr::null_mut();
            self.dev_cap = 0;
        }
        let mut p: *mut c_void = ptr::null_mut();
        if (self.api.cuda_malloc)(&mut p, need) != 0 {
            return false;
        }
        self.dev_buf = p;
        self.dev_cap = need;
        true
    }
}

impl Drop for NvjpegContext {
    fn drop(&mut self) {
        unsafe {
            if !self.dev_buf.is_null() {
                (self.api.cuda_free)(self.dev_buf);
            }
            // v0.8.152 (R3-L3): the six children, in `destroy_children`'s documented reverse-of-
            // creation order — then the handle they were all created from, last.
            destroy_children(
                &self.api,
                self.decoder,
                self.state,
                self.pinned,
                self.device,
                self.stream,
                self.params,
            );
            if !self.handle.is_null() {
                (self.api.destroy)(self.handle);
            }
        }
    }
}

/// Full-res planar-YUV decode result (U2): tight Y/Cb/Cr planes as stored in the bitstream.
/// `w`/`h` = luma dims; `cw`/`ch` = chroma dims (per the SOF's subsampling divisors).
pub struct YuvFrame {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub cw: u32,
    pub ch: u32,
}

/// v0.9.1 (P5, PLAN §65): the nvJPEG implementor of `falcon_decode::ImageDecoder`. Bridges the CUDA
/// hardware JPEG unit into the portable decode seam so the render workers call one trait for both the
/// GPU and CPU paths (and a future macOS `ImageIODecoder` slots in the same way). Per-worker
/// (`&mut self`), like the wrapped [`NvjpegContext`]; the app builds one for the detail worker and one
/// for the ROI worker.
///
/// Layout choice mirrors the three trait shapes:
///   * [`decode_scaled`](ImageDecoder::decode_scaled) — planar YUV first (when the runtime
///     `yuv_enabled` latch is set AND the source fits `target_long`), else the on-device-scaled RGBI
///     path. This serves the detail tier (`target_long = ddim`), whose YUV-miss→RGBI fallthrough is
///     the intended unified path (the Rgb8 is used).
///   * [`decode_yuv`](ImageDecoder::decode_yuv) — native planar YUV **or nothing** (`Err(Unsupported)`,
///     NO RGBI fallthrough): the ROI **YUV tile** route. A YUV miss bails to the RGB source route
///     WITHOUT a wasted full-res RGBI decode (v0.9.2 fix — decode_scaled's fallthrough would have
///     decoded RGBI here only for the ROI call site to discard it).
///   * [`decode_full`](ImageDecoder::decode_full) — native RGBI **only** (never YUV): the ROI RGB
///     source buffer that `crop_region_norm` Lanczos-downscales per tile.
///
/// Any inability to produce pixels (non-JPEG source, unreadable bytes, or the hardware unit rejecting
/// the bitstream) is reported as [`DecodeError::Unsupported`] so the caller `.or_else`s onto the CPU
/// decoder — fallback by return value, never `#[cfg]`.
pub struct NvJpegDecoder {
    ctx: NvjpegContext,
    /// The app's runtime YUV latch (shared `Arc`); flips off if the fused GPU upload pipeline ever
    /// fails to build, at which point this decoder stops emitting planar YUV.
    yuv_enabled: Arc<AtomicBool>,
}

impl NvJpegDecoder {
    /// Wrap an already-initialised context (the app creates the context before wgpu, then hands it
    /// here) with the shared `yuv_enabled` latch.
    pub fn new(ctx: NvjpegContext, yuv_enabled: Arc<AtomicBool>) -> Self {
        Self { ctx, yuv_enabled }
    }

    /// Convenience: create a fresh context AND wrap it; `None` when CUDA/nvJPEG is unavailable.
    pub fn create(yuv_enabled: Arc<AtomicBool>) -> Option<Self> {
        NvjpegContext::new().map(|ctx| Self::new(ctx, yuv_enabled))
    }

    /// v0.9.66 — [`ImageDecoder::decode_scaled`] WITHOUT the planar-YUV attempt.
    ///
    /// The session latch (`yuv_enabled`) lives in this decoder because it is a property of the
    /// GPU pipeline. The other half of the same answer is PER FILE and cannot: `support::yuv_refused`
    /// is the UPLOAD's memory that the fused convert already refused THIS shot's planes once (dims
    /// past the device's limit, or a short plane), so offering them again would refuse again and
    /// loop (v0.8.167, L43 — a per-frame refusal must not de-arm the session). A decoder cannot know
    /// that; the caller can, and this is the door it opens. Byte-identical to the RGBI half of
    /// `decode_scaled` — same context call, same cap, same `Unsupported` on rejection.
    pub fn decode_scaled_rgbi(
        &mut self,
        shot: &Shot,
        target_long: u32,
    ) -> Result<DecodedPixels, DecodeError> {
        if !shot.is_jpeg_source() {
            return Err(DecodeError::Unsupported);
        }
        let bytes = falcon_decode::jpeg_bytes(shot).map_err(|_| DecodeError::Unsupported)?;
        match self.ctx.decode_scaled(&bytes, target_long) {
            Some((data, w, h)) => Ok(DecodedPixels::Rgb8 { data, w, h }),
            None => Err(DecodeError::Unsupported),
        }
    }
}

impl ImageDecoder for NvJpegDecoder {
    fn caps(&self) -> DecodeCaps {
        DecodeCaps { yields_yuv: true }
    }

    fn probe_dims(&mut self, shot: &Shot) -> Option<(u32, u32)> {
        // nvJPEG has no header-only probe of its own; dims are format-derived, so delegate to the
        // shared free function (identical to the CPU decoder's probe).
        falcon_decode::source_dimensions(shot)
    }

    fn decode_scaled(&mut self, shot: &Shot, target_long: u32) -> Result<DecodedPixels, DecodeError> {
        if !shot.is_jpeg_source() {
            return Err(DecodeError::Unsupported); // PNG/TIFF/HEIC/… → CPU
        }
        let bytes = falcon_decode::jpeg_bytes(shot).map_err(|_| DecodeError::Unsupported)?;
        // Planar-YUV first: `target_long` is the max-long cap (0 = native, no cap). None ⇒ oversized
        // source or exotic SOF → the RGBI path below.
        if self.yuv_enabled.load(Ordering::Relaxed) {
            if let Some(f) = self.ctx.decode_full_yuv(&bytes, target_long) {
                return Ok(DecodedPixels::PlanarYuv {
                    y: f.y, cb: f.cb, cr: f.cr, w: f.w, h: f.h, cw: f.cw, ch: f.ch,
                });
            }
        }
        // RGBI, on-device-scaled to ≈ `target_long` (0 = native). None ⇒ hardware rejected the
        // bitstream (e.g. progressive / arch-mismatch) → caller falls to CPU.
        match self.ctx.decode_scaled(&bytes, target_long) {
            Some((data, w, h)) => Ok(DecodedPixels::Rgb8 { data, w, h }),
            None => Err(DecodeError::Unsupported),
        }
    }

    fn decode_full(&mut self, shot: &Shot) -> Result<DecodedPixels, DecodeError> {
        if !shot.is_jpeg_source() {
            return Err(DecodeError::Unsupported);
        }
        let bytes = falcon_decode::jpeg_bytes(shot).map_err(|_| DecodeError::Unsupported)?;
        // Native RGBI (no YUV) — the ROI RGB source buffer.

        match self.ctx.decode_scaled(&bytes, 0) {
            Some((data, w, h)) => Ok(DecodedPixels::Rgb8 { data, w, h }),
            None => Err(DecodeError::Unsupported),
        }
    }

    fn decode_yuv(&mut self, shot: &Shot) -> Result<DecodedPixels, DecodeError> {
        if !shot.is_jpeg_source() {
            return Err(DecodeError::Unsupported); // PNG/TIFF/HEIC/… → RGB source route
        }
        let bytes = falcon_decode::jpeg_bytes(shot).map_err(|_| DecodeError::Unsupported)?;
        // "Planar YUV or nothing" (the ROI YUV tile route's real contract): call ONLY
        // decode_full_yuv, honouring the same `yuv_enabled` latch decode_scaled uses. There is NO
        // RGBI fallthrough — a YUV miss (gray / single-component / exotic-SOF JPEG, or the latch off)
        // returns Unsupported, restoring the old cheap `decode_full_yuv(&bytes, 0)?` bail: decode_full_yuv
        // rejects at the SOF parse (sof_y_sampling) BEFORE any pixel decode, so no wasted full-res
        // RGBI decode + no w*h*3 transient. `0` = native, no cap (the ROI source is always full-res).
        if self.yuv_enabled.load(Ordering::Relaxed) {
            if let Some(f) = self.ctx.decode_full_yuv(&bytes, 0) {
                return Ok(DecodedPixels::PlanarYuv {
                    y: f.y, cb: f.cb, cr: f.cr, w: f.w, h: f.h, cw: f.cw, ch: f.ch,
                });
            }
        }
        Err(DecodeError::Unsupported)
    }
}

/// Read the SOF0/1/2 Y-component sampling factors (h, v) from the raw JPEG —
/// the chroma-plane divisors for standard files whose chroma components are 1×1
/// (422 → (2,1), 420 → (2,2), 444 → (1,1)). None for gray / nonstandard chroma.
pub fn sof_y_sampling(jpeg: &[u8]) -> Option<(usize, usize)> {
    let mut i = 2usize;
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = jpeg[i + 1];
        if m == 0xD8 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        let seg = ((jpeg[i + 2] as usize) << 8) | jpeg[i + 3] as usize;
        if matches!(m, 0xC0 | 0xC1 | 0xC2) {
            let n = *jpeg.get(i + 9)? as usize;
            if n < 3 {
                return None; // grayscale — no chroma planes to size
            }
            // components: [id, h<<4|v, quant] × n, starting at i+10
            let y = *jpeg.get(i + 11)?;
            let (h, v) = ((y >> 4) as usize, (y & 15) as usize);
            // require standard 1×1 chroma so the divisor model holds
            for c in 1..n {
                if *jpeg.get(i + 11 + c * 3)? != 0x11 {
                    return None;
                }
            }
            return (h >= 1 && v >= 1).then_some((h, v));
        }
        i += 2 + seg;
    }
    None
}

/// Choose an nvJPEG scale factor (0=1/1, 1=1/2, 2=1/4, 3=1/8) so the scaled long
/// side is closest to `target`. `full_long <= target` ⇒ no downscale (never upscale).
fn scale_factor(full_long: u32, target: u32) -> c_int {
    if target == 0 || full_long <= target {
        return 0;
    }
    let ratio = full_long as f32 / target as f32;
    (ratio.log2().round() as i32).clamp(0, 3)
}

#[cfg(test)]
mod tests {
    use super::{scale_factor, sof_y_sampling};

    #[test]
    fn scale_factor_picks_nearest_power_of_two() {
        // 8192 px source: 1/2 for a 4096 Reference, 1/4 for a 2048 Fast.
        assert_eq!(scale_factor(8192, 4096), 1);
        assert_eq!(scale_factor(8192, 2048), 2);
        assert_eq!(scale_factor(8192, 1024), 3);
        // Non-power-of-two sources round to the nearest factor (no full decode).
        assert_eq!(scale_factor(8000, 4096), 1);
        assert_eq!(scale_factor(6000, 4096), 1);
        // Never upscale; clamp at 1/8.
        assert_eq!(scale_factor(3000, 4096), 0);
        assert_eq!(scale_factor(40000, 1000), 3);
    }

    /// The v0.9.2 SOF bail, CUDA-free. `decode_yuv` returns `Err(Unsupported)` for a gray / exotic-SOF
    /// JPEG *without* any pixel decode, because `decode_full_yuv` rejects at `sof_y_sampling` before
    /// touching the hardware unit (line ~450). `decode_yuv` itself needs a live NvjpegContext (CUDA),
    /// so this pins the exact parse gate that produces the None → Unsupported (and hence the restored
    /// cheap bail + no wasted RGBI decode); the end-to-end ON/OFF behaviour is the boot-verify's job.
    #[test]
    fn sof_y_sampling_bails_on_grayscale_but_sizes_color() {
        // Minimal grayscale JPEG header: SOI + SOF0 with ncomp = 1 → None (no chroma planes to size).
        let gray = [
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, // SOF0
            0x00, 0x0B, // length = 11
            0x08, // precision
            0x00, 0x10, // height 16
            0x00, 0x10, // width 16
            0x01, // ncomp = 1  (grayscale)
            0x01, 0x11, 0x00, // component 0: id=1, 1×1, quant=0
        ];
        assert_eq!(sof_y_sampling(&gray), None, "grayscale SOF must bail (the decode_yuv Unsupported path)");

        // Minimal 4:2:0 color JPEG header: SOF0 with Y = 2×2 sampling, 1×1 chroma → Some((2, 2)).
        let color420 = [
            0xFF, 0xD8, // SOI
            0xFF, 0xC0, // SOF0
            0x00, 0x11, // length = 17
            0x08, // precision
            0x00, 0x10, // height 16
            0x00, 0x10, // width 16
            0x03, // ncomp = 3
            0x01, 0x22, 0x00, // Y : id=1, h=2 v=2, quant=0
            0x02, 0x11, 0x00, // Cb: id=2, 1×1
            0x03, 0x11, 0x00, // Cr: id=3, 1×1
        ];
        assert_eq!(sof_y_sampling(&color420), Some((2, 2)), "standard 4:2:0 SOF sizes to (2, 2)");
    }
}
