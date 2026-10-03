//! v0.8.148 (E5) — **the hardware HEIC lane goes live**: the boot capability probe, the session
//! pool, and the per-file router that falls soft to WIC for every reason there is.
//!
//! # What this module is, in one sentence
//!
//! E1 reads the container, E2 owns the colour arithmetic, E3 turns a grid of HEVC tiles into a
//! finished photo on the GPU — and all three of them have been INERT since they landed. This is the
//! wire: a `fn(&Path, Option<u32>, Lane) -> Option<(Vec<u8>, u32, u32)>` installed into
//! `falcon_decode`'s [`falcon_decode::HwHeicHook`] slot at boot, which `decode_heic_lane` asks
//! before it asks WIC. Everything else in the epic is already written and already gated.
//!
//! # The direction of the dependency, and why the wire is a function pointer
//!
//! `falcon-hwdec` depends on `falcon-decode` (E1 tells it where the tiles are). `falcon-decode`
//! therefore cannot call it. The shipping app depends on both, so the app installs the door. That
//! keeps `falcon-decode` a pure library — the spikes still link it with no D3D11 anywhere — and it
//! makes "this box has no hardware lane" the ORDINARY case rather than a `cfg` nobody exercises.
//!
//! # Every way this lane declines, and what happens next
//!
//! Nothing here is fatal and nothing here panics. A decline returns `None` and the very next rung of
//! `decode_heic_lane` runs, which is byte-for-byte the path that shipped in v0.8.147:
//!
//! | decline | when |
//! |---|---|
//! | not installed | no D3D11 video device, no HEVC Main/NV12 profile, no GPU adapter, or `FALCON_CLASSIC_HEIC=1` |
//! | not serving | v0.8.149: the lane was LOST mid-session (device removed / assembly gone) — see `lane_lost` |
//! | `container` | E1 will not parse this file, or its tiles do not share one `hvcC` (v0.8.173: remembered per folder) |
//! | `bitdepth` | v0.8.149 (F7): the picture is not 8-bit — a Main10 primary must never enter an 8-bit session (v0.8.173: remembered per folder) |
//! | `geometry` | `irot` is not a quarter turn, or E1's display dims disagree with the assembly's (v0.8.173: remembered per folder) |
//! | `mosaic` | the mosaic is past this device's texture/storage-buffer limits |
//! | `vui` | the SPS VUI names a matrix or a chroma siting E2's kernel refuses to guess at |
//! | `session` | `CreateVideoDecoder` (or the surface array) failed for this tile geometry |
//! | `busy` | every session slot was in use for `HW_WAIT_MS` — the pool is saturated, WIC is idle |
//! | `decode` | the driver failed mid-photo, or the device was lost |
//! | `dims` | the finished photo is not the size the contract promised (belt; the gate is E3-M2's) |
//! | `panic` | v0.8.149 (F2): the decode unwound — the slot is released by `Lease`'s `Drop`, not leaked |
//!
//! # v0.8.149 (E6 fix wave) — what changed, in one paragraph
//!
//! The lane no longer reclassifies the pool posture by being INSTALLED; it reclassifies by being
//! USED. `lane_live()` is still the capability answer, but the prior the posture reads
//! ([`crate::support::heic_fast_accelerated`]) also asks whether the CURRENT FOLDER is being
//! served — because the E6 audit measured what capability alone costs on a folder the lane
//! declines. Three other things followed from that: the lane can now DIE (`lane_lost`), a session
//! slot can no longer leak past a panic (`Lease`), and a post-work refusal is remembered per folder
//! rather than re-bought per tier per visit (`DECLINE_MEMO`).
//!
//! Each parks ONE line per REASON per session on `falcon_decode`'s note channel — the colour
//! round's discipline, and the same channel and drain S1's declines already use. A line per FILE
//! would be the 18-worker flood this codebase keeps not shipping.
//!
//! # THE TIER POLICY
//!
//! Thumb is not routed here AT ALL ([`falcon_decode::hw_heic_lane_applies`]). S1's embedded-preview
//! door costs no HEVC frame whatsoever, and E3-M2's own timing split says why the hardware lane
//! cannot beat it: on this path the tier is nearly free but the DECODE is the floor (40–55 ms of
//! tile decode on a 48 MP photo whether you ask for 8064 px or 256), so a thumbnail served from
//! hardware would cost ~58 ms against a preview read's ~3. Fast and native/detail route.
//!
//! # The non-Windows half
//!
//! There is no hardware lane off Windows on this trunk (the Mac arm is already hardware-fast through
//! Image I/O and is untouched by this epic — plan, RISKS CARRIED). The stubs below answer "no lane"
//! so that every caller is written once, without a `cfg` at the call site.

#[cfg(not(windows))]
pub(crate) fn lane_live() -> bool {
    false
}
#[cfg(not(windows))]
pub(crate) fn probe_and_arm() {}
#[cfg(not(windows))]
pub(crate) fn note_render_storage_limit(_bytes: u64) {}
/// v0.8.149 (F9/B8): no probe off Windows, so the boot posture line has nothing pending.
#[cfg(not(windows))]
pub(crate) fn probe_pending() -> bool {
    false
}
/// v0.8.149 (F9/A10): no pool off Windows, so no churn to report.
#[cfg(not(windows))]
pub(crate) fn session_churn_report() -> Option<String> {
    None
}
/// v0.8.149 (F8): no decline memo off Windows.
#[cfg(not(windows))]
pub(crate) fn note_folder_swap() {}

/// v0.8.165 (WAVE 1): no hardware lane off Windows, so no GPU-coloured HEIC frame either — the
/// detail tier falls straight through to the path it has always taken.
#[cfg(not(windows))]
pub(crate) fn decode_full_managed(
    _path: &std::path::Path,
    _scale_to: Option<u32>,
    _src: falcon_color::Gamut,
    _dst: falcon_color::Gamut,
) -> crate::support::ManagedAnswer {
    crate::support::ManagedAnswer::Declined
}
/// v0.8.172 (G6): no hardware lane off Windows, so no file can be routed to an enlarged ask — the
/// one-decode gate's per-file pre-check answers a flat no and the arm never engages.
#[cfg(not(windows))]
pub(crate) fn lane_admits(_path: &std::path::Path, _scale_to: Option<u32>) -> bool {
    false
}
/// v0.8.165: no renderer-limit pre-check to feed off Windows.
#[cfg(not(windows))]
pub(crate) fn note_render_max_texture(_dim: u32) {}
/// v0.8.171: no hardware tile grid off Windows, so nothing to abort — the guard is a unit value and
/// the browse workers arm it unconditionally, exactly as they do on Windows.
#[cfg(not(windows))]
pub(crate) struct Supersession;
#[cfg(not(windows))]
pub(crate) fn watch_supersession(_f: Box<dyn Fn() -> bool>) -> Supersession {
    Supersession
}
#[cfg(windows)]
pub(crate) use win::{
    decode_full_managed, lane_admits, lane_live, note_folder_swap, note_render_max_texture,
    note_render_storage_limit, probe_and_arm, probe_pending, session_churn_report,
    watch_supersession,
};

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use falcon_decode::{note_decode_once, DecodeRoute, HwHeicAnswer, Lane, SrcKind};
    use falcon_gpu::heic::{FinishOut, HeicAssembler};
    use falcon_hwdec::{photo, DecodeDevice, HwDecError, PhotoDecoder, PhotoRun, TileSource};

    use crate::support::{self, log_event};

    /// How many decoder sessions may exist at once, across every tile geometry.
    ///
    /// It is a VRAM and driver-object bound, not a throughput knob. The GPU has ONE video decode
    /// engine: E3-M2 measured a 48 MP photo at 132–148 ms end to end of which 40–55 ms is tile
    /// decode, so concurrency beyond a couple of sessions cannot make the engine go faster — what it
    /// buys is that one session's GPU assembly (compute) overlaps the next session's decode instead
    /// of serialising behind it. Three is two of those plus one held in reserve for the interactive
    /// tier (see [`session_budget`]). Each session costs a D3D11 device, one `ID3D11VideoDecoder`
    /// and [`SURFACES`] NV12 slices — about 11 MB of VRAM at a 48 MP file's 896×1024 tiles.
    const MAX_SESSIONS: usize = 3;

    /// Decode surfaces per session — the pipelining window, and the resident NV12 working set.
    /// E3-M2's timing rows were measured at 8 and the streaming shape bounds system memory to one
    /// tile regardless, so this is the number that measurement describes.
    const SURFACES: u32 = 8;

    /// How long a decode may wait for a session slot before it gives up and lets WIC have the file.
    ///
    /// # v0.8.149 (F6) — RE-DERIVED FROM THE CONTENDED QUEUE, because the old arithmetic used the
    /// uncontended cost and therefore described a queue that does not exist
    ///
    /// v0.8.148 set this at 5 000 ms and justified it like this: *"The worst legitimate queue is the
    /// 18-worker pool all wanting HEIC at once: 18 frames over 2 speculative slots at ~130 ms is
    /// ~1.2 s, comfortably inside this."* Both halves of that are wrong in the same way. The ~130 ms
    /// is E3-M2's SOLO end-to-end figure for one 48 MP photo on an idle box; the queue it is being
    /// multiplied through is by definition the one where three sessions, one video engine and one
    /// compute queue are all saturated, and per-photo SERVICE time there is several times the solo
    /// number. Feed the same arithmetic the contended figure and ~1.2 s becomes ~4–5 s — which is
    /// not "comfortably inside" a 5 000 ms bound, it IS the bound. The E6 audit measured exactly
    /// that: waits peaking at 4.3–5.1 s against a 5.0 s cliff, i.e. the ordinary flood was running
    /// one noisy neighbour away from tripping a bound that was supposed to be unreachable, and under
    /// 3× contention it did trip — at a 7 s cost for that file, because a `busy` decline pays the
    /// whole wait AND THEN the WIC decode it was avoiding.
    ///
    /// # THE RECALIBRATION WAS ATTEMPTED, MEASURED, AND REVERTED — with the numbers
    ///
    /// The rule the recalibration was derived from is sound and it is worth writing down:
    ///
    /// > **the wait must never be able to cost more than the decode it is avoiding.**
    ///
    /// What was wrong was the number fed into it. The E6 audit weighed the wait against a CONTENDED
    /// WIC decode of 2 400–3 100 ms (the v0.8.114 regime data), which put the bound in the
    /// 1 500–2 000 ms class. But that 2 400–3 100 ms was measured under the COSTLY posture's 4-slot
    /// cap — and by the time a `busy` decline can fire at all, F1 has promoted the folder and the
    /// cap is OFF, so the file that falls soft meets 18-way contention, not 4-way.
    ///
    /// So it was built at 2 000 ms and run on the 100-file 48 MP flood (`FALCON_AUTOSCRUB=12`,
    /// `e5_heicflood`), and the pool's own new instrument reported what happened:
    ///
    /// ```text
    /// heic sessions: 40 created, 37 retired (geometry churn), 3 live of 3;
    ///                94 checkout(s) waited, worst 2131 ms, 7 gave up at 2000 ms and fell soft to WIC
    /// decode-stats fast HEIC/hw  n=92 med=918ms  max=1692ms
    /// decode-stats fast HEIC/wic n=7  med=5976ms max=6298ms
    /// ```
    ///
    /// Seven files that a longer wait would have served from hardware in ~918 ms instead cost
    /// **5 976–6 298 ms** each, because a fall-soft at exactly the moment the pool is saturated is a
    /// CPU HEVC decode against seventeen others. The same run at 5 000 ms (E5's, same folder, same
    /// lever) produced **zero** `busy` declines. Measured against its own criterion, the shorter
    /// bound loses by ~6.5×: waiting IS the cheap option on this pool shape, because the queue
    /// drains — the sessions are busy, not wedged.
    ///
    /// # …and the number is still not right, which is a different item
    ///
    /// The audit's complaint stands: the legitimate 18-worker queue reaches 4.3–5.1 s against this
    /// 5.0 s cliff, so the bound really is one noisy neighbour away from firing, and when it fires it
    /// costs the wait PLUS the decode. Neither direction fixes that, because a TIME bound cannot
    /// tell a busy pool from a wedged one — both look like "no slot yet". The bound that can is a
    /// PROGRESS bound: a driver that is merely busy keeps RETURNING slots, so "no session has been
    /// checked in by anybody for T" is the wedged-driver signal, and it can be short (a second or
    /// two) without ever firing on a queue that is draining. That is a design change with its own
    /// falsifier, not a constant to retune, so it is recorded here and left for the wave that takes
    /// it rather than improvised into a fix wave.
    ///
    /// [`session_churn_report`] prints the observed wait distribution on the ordinary flush cadence,
    /// which is what makes that next derivation a measurement instead of another comment.
    ///
    /// (The old note's arithmetic — "18 frames over 2 speculative slots at ~130 ms is ~1.2 s,
    /// comfortably inside this" — is withdrawn. ~130 ms is E3-M2's SOLO figure; the queue it was
    /// multiplied through is by definition the contended one. At the measured contended service time
    /// the same arithmetic gives ~4–5 s, which is the bound itself and not comfortably inside it.)
    const HW_WAIT_MS: u64 = 5_000;

    // ── the lane's live state ────────────────────────────────────────────────────────────────────

    /// Set exactly once, by [`arm`], when the probe has PROVED the whole chain: a D3D11 video device
    /// that decodes HEVC Main to NV12, and a GPU adapter whose device holds a 48 MP mosaic.
    ///
    /// Read by [`crate::support::heic_fast_accelerated`], which is what turns the costly-lane
    /// classifier off for HEIC. It is a one-way latch and it is set BEFORE the hook is installed, so
    /// there is no window in which the posture calls the lane cheap while the router still declines
    /// everything.
    static LANE_LIVE: AtomicBool = AtomicBool::new(false);

    /// v0.8.149 (F1) — THE DE-LATCH. Set once, by [`lane_lost`], when the hardware went away under
    /// a decode. From that instant [`lane_live`] is false for the rest of the process.
    ///
    /// The E6 audit's convergent class was "live-but-irreversible": v0.8.148 had no way back at all.
    /// `LANE_LIVE` was a one-way latch, the assembler is a `OnceLock` with no device-lost callback
    /// and no rebuild, and the posture read the latch — so a lost assembly device left the app
    /// paying WIC's cost with WIC's protections disarmed, for ever, with nothing in the log to say
    /// what had happened. This is the way back, and it is deliberately blunt: no retry, no rebuild,
    /// no half-alive state. A device that has gone away once during a decode is not a device to keep
    /// offering photographs to, and the fall-back — v0.8.147 — is a shipped, measured configuration.
    static LANE_LOST: AtomicBool = AtomicBool::new(false);

    /// Is the hardware HEIC lane live in this process?
    ///
    /// FALSE until the boot probe lands (a few hundred ms in, on its own thread), and FALSE forever
    /// after on a box without the hardware. The pre-probe answer is the CONSERVATIVE one — HEIC
    /// reads costly, the pool posture narrows exactly as it did in v0.8.147.
    ///
    /// v0.8.149 (F1): …and FALSE again for the rest of the session after a device loss
    /// ([`LANE_LOST`]). Two latches, one AND, both moving in the safe direction — off before the
    /// probe has proved the chain, off again the moment the chain breaks.
    #[inline]
    pub(crate) fn lane_live() -> bool {
        LANE_LIVE.load(Ordering::Relaxed) && !LANE_LOST.load(Ordering::Relaxed)
    }

    /// v0.8.149 (F1): kill the lane for the session and SAY SO, once.
    ///
    /// Order matters and it is the reverse of [`arm`]'s: the latch is set BEFORE the line is logged,
    /// so no thread can read a live lane after a reader has been told it is dead. The hook stays
    /// installed — `falcon_decode`'s `OnceLock` cannot be un-set — and it does not need to be: the
    /// router's first line re-asks `lane_live()`, so from here every call returns `None` at zero
    /// cost and `decode_heic_lane` runs the v0.8.147 ladder.
    ///
    /// v0.8.151 (F1): the call is [`support::hw_lane_lost`], not `hw_lane_lost_line`. v0.8.149
    /// logged the sentence and invalidated nothing, so the pool posture kept the relaxed
    /// hardware-era prefetch runway on a lane that had just become software WIC — the one
    /// protection the sentence names first. One function now does both, so no future edit can log
    /// the promise without keeping it.
    fn lane_lost(reason: &str) {
        if LANE_LOST.swap(true, Ordering::Relaxed) {
            return;
        }
        log_event(&support::hw_lane_lost(reason));
    }

    /// v0.8.149 (F1/F9-B8): will a hardware-lane probe run at all this session?
    ///
    /// Read by the BOOT `pool posture:` line, which is printed before the probe has answered. On a
    /// Windows build with neither switch armed the line's HEIC claims are provisional, and saying so
    /// costs one clause; the alternative is a log whose first posture statement is contradicted a
    /// few hundred ms later by the probe with nothing to connect the two.
    pub(crate) fn probe_pending() -> bool {
        !falcon_decode::classic_heic()
            && !hw_disabled_from_env(std::env::var("FALCON_HW_HEIC").ok().as_deref())
    }

    /// The renderer device's storage-binding limit in bytes, published by
    /// [`note_render_storage_limit`], so the topology verdict quotes THIS machine instead of a spec.
    ///
    /// It has a SEPARATE captured flag rather than a 0 sentinel, and that is not pedantry: this
    /// round's first boot-verify measured the value and it IS zero. Slint's `WGPUSettings::default()`
    /// asks for downlevel-friendly limits, and a device that promises WebGL2 compatibility promises
    /// no storage buffers at all. Conflating "not measured yet" with "measured, and the answer is
    /// none" would have thrown away the strongest number the topology decision has.
    static RENDER_STORAGE_LIMIT: AtomicU64 = AtomicU64::new(0);
    static RENDER_CAPTURED: AtomicBool = AtomicBool::new(false);

    // ── v0.8.171 (HEIC SPEED PRIORITY): THE SUPERSESSION SIGNAL ─────────────────────────────────
    //
    // WHY A THREAD-LOCAL AND NOT A PARAMETER. The question is "has the WORKER RUNNING THIS DECODE
    // moved on?", and its answer is different for each of the two workers that can ask it: the fast
    // pool compares against `support::DROP_DIST` (110, fixed) with the hover preview's one
    // exemption (`support::fast_job_stale`, 2026-09-07), the detail worker against the LIVE
    // `detail_ahead_atomic` with a compare-mode exemption. So it cannot be a constant, and it cannot
    // be a shared atomic either — it is a closure over the worker's own state. The route to this
    // module from those workers runs through `falcon_decode`'s hook, which is a bare `fn` pointer
    // (deliberately: it is what keeps `falcon-decode` a library the spikes can still link, with no
    // dependency back on this crate), and a `fn` pointer cannot close over anything. Threading a
    // predicate parameter through instead would put it on `browse_frame_rgba` and every one of its
    // callers, including ~18 rows in `tests/heic.rs` that have nothing to do with supersession.
    //
    // A thread-local is the honest shape for this specific question: the decode runs SYNCHRONOUSLY
    // on the worker's own thread, between the worker arming the signal and the worker disarming it,
    // so "this thread's current job" is exactly the scope the value has.
    //
    // L43 — WHAT UN-SETS IT: [`Supersession`]'s `Drop`, unconditionally, including on an unwind
    // through the decode. There is no other writer and no timer. A worker that arms one and panics
    // still leaves the slot empty for its own next job, which matters because these threads are
    // reused for the life of the process.
    thread_local! {
        static SUPERSEDED: std::cell::RefCell<Option<Box<dyn Fn() -> bool>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// v0.8.171: arm this thread's supersession signal for the life of the returned guard.
    ///
    /// The caller passes its OWN "have I moved on?" test — the fast pool's chase-latest distance,
    /// the detail worker's `jumped_away` radius — and the hardware lane asks it between tile chunks.
    /// Nothing else reads it; nothing else may write it.
    ///
    /// v0.8.172 (B-Y14) — **IT DOES NOT NEST, AND THAT IS A CONTRACT RATHER THAN AN OVERSIGHT.** An
    /// inner `watch_supersession` would REPLACE the outer predicate and its guard's `Drop` would
    /// then leave the slot EMPTY while the outer job was still running — silently un-arming a worker
    /// that believes it is armed. Save/restore semantics were considered and rejected: they would
    /// make the type carry an `Option<Box<dyn Fn>>` to serve a case that cannot occur, and hiding a
    /// nesting bug behind correct-looking behaviour is worse than not having one. The invariant that
    /// makes it safe is structural: each of the two browse workers arms exactly ONE guard at the top
    /// of one job and the decode runs synchronously beneath it, so a second arming on the same thread
    /// would mean a worker had started a second job inside its first. Any future caller must hold to
    /// that.
    #[must_use = "the signal is disarmed when the guard drops — binding it to `_` disarms it now"]
    pub(crate) fn watch_supersession(f: Box<dyn Fn() -> bool>) -> Supersession {
        SUPERSEDED.with(|c| *c.borrow_mut() = Some(f));
        Supersession
    }

    /// The disarm, by construction. See [`watch_supersession`].
    pub(crate) struct Supersession;
    impl Drop for Supersession {
        fn drop(&mut self) {
            SUPERSEDED.with(|c| *c.borrow_mut() = None);
        }
    }

    /// Ask this thread's signal. FALSE when none is armed — every caller that has not opted in
    /// (the thumb tier, the ROI tier, every test, every non-browse door) behaves exactly as it did
    /// in v0.8.170, without knowing this exists.
    fn is_superseded() -> bool {
        SUPERSEDED.with(|c| c.borrow().as_ref().is_some_and(|f| f()))
    }

    /// v0.8.148 (E5) — THE DEVICE-TOPOLOGY MEASUREMENT, taken from the app's OWN renderer device.
    ///
    /// Called from the `RenderingSetup` notifier with `device.limits().max_storage_buffer_binding_size`.
    /// The assembly could in principle share that device (`HeicAssembler::on_device` exists precisely
    /// so E5 could pass it), and the question is decided by one number: Slint builds its device from
    /// `WGPUSettings::default()`, i.e. wgpu's DEFAULT limits, while a 48 MP mosaic's RGB is a 198 MB
    /// storage buffer. Recording the real figure means the verdict line is a measurement of this
    /// machine rather than a quotation.
    pub(crate) fn note_render_storage_limit(bytes: u64) {
        RENDER_STORAGE_LIMIT.store(bytes, Ordering::Relaxed);
        RENDER_CAPTURED.store(true, Ordering::Relaxed);
        maybe_log_topology();
    }

    /// The assembly device's own storage-binding limit, published by the probe. 0 = not probed.
    static ASSEMBLY_STORAGE_LIMIT: AtomicU64 = AtomicU64::new(0);

    /// v0.8.148 (E5): log the topology verdict ONCE, when BOTH limits are known — and let whichever
    /// of the two arrives second be the one that does it.
    ///
    /// The ordering is genuinely racy and not in anybody's gift: the probe runs on its own thread
    /// right after the backend selection, while `RenderingSetup` fires when Slint first renders. The
    /// first boot-verify of this round measured the probe landing 407 ms in and the renderer's limits
    /// arriving after it, so a verdict emitted inside the probe would have said "not captured" on
    /// every ordinary boot — a topology decision documented by a line that never carries its
    /// measurement. Hence the latch: two facts, one sentence, printed when the second lands.
    fn maybe_log_topology() {
        static SAID: AtomicBool = AtomicBool::new(false);
        let asm = ASSEMBLY_STORAGE_LIMIT.load(Ordering::Relaxed);
        if asm == 0 || !RENDER_CAPTURED.load(Ordering::Relaxed) {
            return;
        }
        if SAID.swap(true, Ordering::Relaxed) {
            return;
        }
        let render = RENDER_STORAGE_LIMIT.load(Ordering::Relaxed);
        log_event(&format!("heic hw topology: {}", topology_verdict(asm, Some(render))));
    }

    /// The RGB storage buffer a 48 MP iPhone HEIC's mosaic needs: 8064 × 6144 × 4. Quoted in the
    /// topology verdict so the two limits are compared against a real photo and not an abstraction.
    const MOSAIC_48MP_BYTES: u64 = 8064 * 6144 * 4;

    // ── the boot probe ───────────────────────────────────────────────────────────────────────────

    /// The falsifier lever, and the honest name for it.
    ///
    /// `FALCON_HW_HEIC=0` makes the probe answer exactly as a box with no video device answers — the
    /// hook is never installed, `lane_live()` stays false, every HEIC falls to WIC and the posture
    /// stays costly. It exists so "the app is healthy when the capability probe says no" is a thing
    /// that can be RUN on this machine rather than argued about, and it is read through a pure parse
    /// for the same reason [`falcon_decode::classic_heic_from_env`] is.
    #[inline]
    pub(crate) fn hw_disabled_from_env(v: Option<&str>) -> bool {
        v == Some("0")
    }

    /// The SECOND falsifier lever: `FALCON_HW_HEIC_DECLINE=<substring>` makes the router decline
    /// every file whose NAME contains that substring, exactly as a real per-file decline does.
    ///
    /// It exists because the interesting failure is not "no hardware" — that is one boolean and the
    /// lever above covers it — but "this file declines and its NEIGHBOURS do not". That is the shape
    /// of every real decline (a container E1 will not parse, a VUI the kernel refuses, a mosaic past
    /// the device's limits), it is the one that has to leave a browse healthy, and without a lever it
    /// cannot be run at all on a corpus where every file happens to be decodable. `FALCON_SIM_DEVICE_LOST`
    /// is the precedent: a dev instrument that drives a real path rather than a comment claiming it
    /// would work.
    ///
    /// A SUBSTRING of the file name, not a path: it has to be typeable into a PowerShell one-liner.
    #[inline]
    pub(crate) fn hw_decline_injected(file_name: &str, pattern: Option<&str>) -> bool {
        match pattern {
            Some(p) if !p.is_empty() => file_name.contains(p),
            _ => false,
        }
    }

    // ── v0.8.165 (WAVE 1): the GPU-colour arm and its lever ─────────────────────────────────────

    /// `FALCON_HEIC_GPU_COLOR=0` reverts the hardware lane's DETAIL frames to the CPU colour path
    /// wholesale — the assembler finishes in the file's own gamut exactly as v0.8.164 shipped it,
    /// `browse_frame_rgba` expands RGB→RGBA and `falcon_color::transform_rgba` converts.
    ///
    /// Default ON: the GPU arm is the product. The lever exists for three things — the A/B of the
    /// measurement pass that follows this wave, a field escape hatch on a weak iGPU, and the
    /// falsifier that lets "the CPU arm is still healthy" be RUN rather than argued. It parses
    /// EXACTLY `=0`, the [`hw_disabled_from_env`] / `classic_heic_from_env` precedent: a variable
    /// set to anything else is not a disable.
    #[inline]
    pub(crate) fn gpu_color_disabled_from_env(v: Option<&str>) -> bool {
        v == Some("0")
    }

    /// Is the GPU colour arm ARMED this session? Capability (the lane) is asked separately, per
    /// file, by the router — this is only the lever.
    fn gpu_color_armed() -> bool {
        static ARMED: OnceLock<bool> = OnceLock::new();
        *ARMED.get_or_init(|| {
            !gpu_color_disabled_from_env(std::env::var("FALCON_HEIC_GPU_COLOR").ok().as_deref())
        })
    }

    // L43 — WHAT UN-SETS THE GPU COLOUR ARM. Nothing here is a latch, and that is deliberate: the
    // arm is live iff `gpu_color_armed()` (a process-lifetime env read, immutable by construction)
    // AND `lane_live()` — which v0.8.149's `lane_lost` turns OFF the moment the hardware goes away.
    // So a lost device demotes the colour arm in the same instant it demotes the lane, through the
    // existing de-latch, and there is no new state that could outlive its own justification. The
    // per-file gates (the renderer limit, the mosaic, the pool) are re-asked on every attempt and
    // remember nothing except the two POST-WORK refusals the v0.8.149 memo already bounds by
    // folder generation.

    /// v0.8.165 (iGPU deliverable 2): the RENDERER device's own `max_texture_dimension_2d`,
    /// published at `RenderingSetup` beside the storage limit. 0 = not captured yet.
    ///
    /// It is here, not a constant, because that is the entire point: this box reports 32768 and an
    /// Intel iGPU commonly reports 16384. A GPU-coloured frame goes STRAIGHT into a renderer
    /// texture with no CPU stage that could resize it, so the frame must be refused on the assembly
    /// side — before the whole hardware budget is spent — if the renderer could not hold it.
    static RENDER_MAX_TEX: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn note_render_max_texture(dim: u32) {
        RENDER_MAX_TEX.store(u64::from(dim), Ordering::Relaxed);
    }

    /// Would the renderer hold a `w × h` texture? `None` when the limit has not been captured yet
    /// (the probe can land before the first frame does) — the caller treats that as "do not risk
    /// it", which costs one folder's first frames the GPU arm and never a wrong picture.
    ///
    /// FALSIFIER (L28): return `Some(true)` when the limit is 0 and
    /// `the_renderer_precheck_refuses_what_it_has_not_measured` reddens.
    #[inline]
    pub(crate) fn renderer_holds(w: u32, h: u32, limit: u64) -> Option<bool> {
        if limit == 0 {
            return None;
        }
        Some(u64::from(w) <= limit && u64::from(h) <= limit)
    }

    fn injected_decline_pattern() -> Option<&'static str> {
        static PAT: OnceLock<Option<String>> = OnceLock::new();
        PAT.get_or_init(|| {
            std::env::var("FALCON_HW_HEIC_DECLINE").ok().filter(|s| !s.is_empty())
        })
        .as_deref()
    }

    /// v0.8.149 (F2): THE THIRD falsifier lever — `FALCON_HW_HEIC_PANIC=<substring>` panics the
    /// decode of every file whose NAME contains that substring, AFTER the session has been checked
    /// out.
    ///
    /// It exists because the leak the [`Lease`] guard closes is otherwise unreachable on a healthy
    /// corpus: nothing in the shipping path panics, so "the slot is released on unwind" would be a
    /// claim about code nobody had ever run. Post-checkout is the whole point — a panic before the
    /// checkout leaks nothing and would test the wrong thing.
    ///
    /// It shares [`hw_decline_injected`]'s parse (a substring of the file NAME, typeable into a
    /// PowerShell one-liner) for the same reason the decline lever does, and it is the same family
    /// as `FALCON_SIM_DEVICE_LOST`: a dev instrument that drives a REAL path, rather than a comment
    /// claiming the path would work.
    fn injected_panic_pattern() -> Option<&'static str> {
        static PAT: OnceLock<Option<String>> = OnceLock::new();
        PAT.get_or_init(|| std::env::var("FALCON_HW_HEIC_PANIC").ok().filter(|s| !s.is_empty()))
            .as_deref()
    }

    // ── v0.8.149 (F8): the POST-WORK decline memo ────────────────────────────────────────────────
    //
    // Six of the nine declines are arithmetic — `container`, `geometry`, `mosaic`, `vui` and the two
    // pool ones — and they cost a file read and a parse. Two are not: `dims` and `decode` are
    // reached only AFTER a session has been taken out of a pool of three, every tile has been
    // marshalled and submitted, and the GPU has assembled the picture. A file that declines THERE
    // has spent the entire hardware budget to produce nothing, and v0.8.148 made it spend it again
    // on every visit at every tier: fast on the way past, native when the user stops, fast again on
    // the way back. A folder with one such file pays that repeatedly, and the log says nothing after
    // the first note because the note is once per REASON.
    //
    // So the outcome is remembered, keyed by (folder generation, path). The GENERATION is what
    // bounds it: `note_folder_swap` clears the map at the same chokepoint `reset_fast_cost` runs
    // from, so the memo can never outlive the folder that taught it, a re-scan of a folder whose
    // files have been replaced re-asks the hardware, and the map cannot grow across a long session
    // of swaps.
    //
    // v0.8.173 (H4) — AND THE THREE PARSE-TIME REFUSALS JOIN THEM, because "cheap" stopped being
    // true when the parse acquired a second caller. `container`, `bitdepth` and `geometry` are not
    // arithmetic: each costs an ~11 MB read and a full container parse, and since v0.8.172 (G6)
    // they are paid TWICE per ask on a serving folder — once by `lane_admits`, the one-decode gate's
    // pre-check, and once by the `route()` that follows it — then again per tier and per visit.
    // Their verdicts are deterministic in the file (it parses or it does not; its `irot` is a
    // quarter turn or it is not), so remembering them is the same trade the two post-work refusals
    // make, bounded by the same generation.
    //
    // `mosaic` and `vui` are still NOT remembered, and that is the line: both are arithmetic over a
    // source the plan cache is already holding, so the memo would save nothing but a comparison.
    // The two pool refusals (`busy`, `session`) are not remembered because they are facts about the
    // INSTANT, not about the file — memoising either would exile a good photograph over a moment's
    // saturation, which is the opposite of what this memo is for.
    //
    // v0.8.167 (audit) — AND IT IS REMEMBERED PER DOOR. v0.8.165 gave the router a second caller:
    // `decode_full_managed`, whose finish pass converts colour on the assembly device. The two
    // doors share every gate, so one memo looked right — but they do NOT fail in the same places.
    // The managed door alone asks the RENDERER's texture limit, and its `decode_managed` runs a
    // colour pass the source door never runs, so a refusal that is TRUE of the managed finish need
    // not be true of the source one. Filed against the path alone, one managed failure demoted the
    // file all the way to WIC: the SOURCE-door hook would find the memo, decline, and the hardware
    // decode this box is perfectly capable of would never be attempted again in that folder.
    //
    // The relation between the doors is an ORDER, not a symmetry, and [`DeclineMemo::blocks`]
    // encodes exactly that: a SOURCE-door failure blocks both (the tiles, the composite and the
    // dims contract are shared, so the managed door would meet the same wall one pass later),
    // while a MANAGED-door failure blocks only the managed door and leaves the file on the
    // hardware lane with CPU colour — which is the fall-soft Wave 1 designed and the log claims.
    //
    // v0.8.168 (F4) — THE SCOPE FOLLOWS THE FAULT, NOT THE DOOR THAT MET IT. The mechanism above is
    // right; v0.8.167's WRITERS were not. Both of them filed under `out` — the door the attempt
    // happened to be running as — and both refusals they file are refusals of the SHARED half:
    //
    //   * `dims` compares the produced photo against `photo::output_dims(&src, scale_to)`, which is
    //     computed at step (2) BEFORE `out` is consulted and is byte-identical for both doors; the
    //     finish pass derives its own `ow`/`oh` from the same geometry. A managed dims mismatch is
    //     therefore evidence about the source door too.
    //   * `decode` is the whole of `decode_impl`: geometry, VUI, canvas, tile marshalling,
    //     compositing and finish. `decode_managed` differs from `decode` in ONE argument, consulted
    //     only in the last pass — so `decode`'s failure set is a superset of the managed-only part.
    //
    // Filed per door, the split therefore never fired for the class it was designed for, and it
    // cost a real thing in the other direction: a SHARED fault met first at the managed door (an
    // `HwDecError::Api` on a tile, say) left the source slot empty, so the very next tier bought a
    // second full hardware attempt — checkout, tiles, composite — to rediscover it.
    //
    // COULD THE MANAGED DOOR EVER PROVE A COLOUR-ONLY FAULT? The taxonomy was inspected and the
    // answer today is no. `HwDecError` has no colour-pass variant: everything the managed arm can
    // fail at that the source arm cannot — the `src_to_dst_matrix` packing, the custom inverse-LUT
    // texture, the 4-bytes-per-pixel unpack — surfaces from `finish_out` as `HwDecError::Assembly`,
    // which is exactly what a SHARED assembly fault surfaces as, and which stands the whole lane
    // down (`lane_lost`) before the memo could matter anyway. So both writers file BOTH DOORS, and
    // the `managed` slot below stays for the day a colour-specific variant exists — with the rule
    // stated here rather than rediscovered from behaviour.
    #[derive(Default)]
    struct DeclineMemo {
        /// A remembered refusal whose FAULT is in the half both doors share. Blocks both.
        /// (Named for the source door because that is the door whose every failure is shared.)
        source: Option<&'static str>,
        /// A remembered refusal provably confined to the MANAGED door's colour arm. Blocks the
        /// managed door only. v0.8.168: no production writer can prove this yet — see the block
        /// above — so this slot is written only by `DeclineMemo`'s own row today.
        managed: Option<&'static str>,
    }

    impl DeclineMemo {
        /// The remembered refusal that blocks `out`, if any.
        fn blocks(&self, out: FinishOut) -> Option<&'static str> {
            match out {
                FinishOut::SourceRgb => self.source,
                FinishOut::ManagedRgba { .. } => self.source.or(self.managed),
            }
        }

        /// File one refusal under the SCOPE OF ITS FAULT — `SourceRgb` for a fault in the shared
        /// half (which blocks both doors), `ManagedRgba` for one confined to the colour arm.
        /// v0.8.168 (F4): this used to be handed the door the attempt was running as, which is a
        /// different question and the wrong one.
        ///
        /// ── v0.8.187 (Y6): THE OBLIGATION A `ManagedRgba` WRITER TAKES ON ────────────────────────
        /// The scope arrives as a full `FinishOut`, and `ManagedRgba` CARRIES `{ src, dst }` — but
        /// the `managed` slot below stores only a reason string, so the destination is discarded.
        /// That is sound TODAY for one reason and one reason only: no production writer files this
        /// slot at all (see the v0.8.168 block above — every reachable fault is shared, so both
        /// writers file `SourceRgb`), so the slot is written by this module's own test row and by
        /// nothing else. The moment a colour-specific `HwDecError` variant exists and a real writer
        /// appears, the recorded refusal becomes destination-dependent — "this file's managed finish
        /// failed converting P3 -> Adobe RGB" is not evidence about the same file into sRGB — and a
        /// `dst`-blind memo would decline the managed door for every output gamut the user switches
        /// to afterwards, for the rest of the folder. THE OBLIGATION: that writer must widen this
        /// slot to carry the `dst` it refused for (`Option<(Gamut, &'static str)>` and a `blocks`
        /// that compares it against the live destination), not merely start calling `note`.
        fn note(&mut self, scope: FinishOut, reason: &'static str) {
            match scope {
                FinishOut::SourceRgb => self.source = Some(reason),
                FinishOut::ManagedRgba { .. } => self.managed = Some(reason),
            }
        }
    }
    /// ── v0.8.187 (FRESH-1, ledger L42): THE MEMO IS KEYED `(path, mtime)`, NOT `path` ────────────
    /// Its neighbour `PLAN_CACHE` writes the rule down twenty lines below — "the key is
    /// `(path, mtime)`, never the path alone. A file rewritten under the same name in an open folder
    /// (a phone still syncing, an export overwriting) revalidates and re-parses" — and this memo,
    /// which is bound at the SAME chokepoint by the SAME generation and answers about the SAME
    /// files, carried only the name. The consequence is smaller than the plan cache's (a stale
    /// entry costs the hardware lane for one file, it does not serve wrong pixels) but it is the
    /// same class: rewrite a HEIC in place with a file the device CAN decode and the refusal
    /// recorded against the old bytes kept it on WIC for the rest of the folder.
    ///
    /// A file whose `metadata()` cannot be read is NOT memoised — no mtime, no key, no entry — which
    /// is the plan cache's rule too: the live path runs and answers for itself.
    static DECLINE_MEMO: Mutex<Option<(u64, HashMap<PathBuf, (std::time::SystemTime, DeclineMemo)>)>> =
        Mutex::new(None);
    static FOLDER_GEN: AtomicU64 = AtomicU64::new(0);

    /// The mtime this file's memo entry is keyed to, or `None` when the metadata read fails.
    /// Shared by the read and the write so the two can never key on different things.
    fn decline_mtime(path: &Path) -> Option<std::time::SystemTime> {
        std::fs::metadata(path).and_then(|m| m.modified()).ok()
    }

    /// v0.8.149 (F8): a new folder is being applied — the memo's generation moves and it empties.
    /// Called from the ONE folder-swap chokepoint, beside `support::reset_fast_cost`.
    ///
    /// v0.8.157 (§3): …and the tile-plan cache empties with it, on the same generation and at the
    /// same chokepoint, for the same reason. See [`PLAN_CACHE`].
    pub(crate) fn note_folder_swap() {
        FOLDER_GEN.fetch_add(1, Ordering::Relaxed);
        *DECLINE_MEMO.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *PLAN_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // ── v0.8.157 (§3): THE TILE-PLAN CACHE (ruled 5.2b, 2026-08-05) ─────────────────────────────
    //
    // WHAT IT REMOVES. Step (1) of the router is `falcon_hwdec::tile_source`, which is
    // `fs::read` of the WHOLE file (~11 MB on a 48 MP iPhone photo) + `parse_heif_grid` +
    // `parse_hvcc` + the bit-depth gates. Every attempt paid it in full, and one photograph is
    // attempted repeatedly by design: fast on the way past, native when the user stops, fast again
    // on the way back — and, until §2 landed, once per retry of a decode that could never succeed.
    //
    // AND R3-M3'S REPEAT EXPOSURE. The hardening round's R3-M3 is the `ipma` map rebuilt per parse
    // inside `parse_heif_grid`; whatever its per-parse cost, this removes the REPEATS of it, which
    // is the half the 08-05 ruling named ("also removes R3-M3's repeat exposure").
    //
    // L42 — NAME vs BYTES. The key is `(path, mtime)`, never the path alone. A file rewritten under
    // the same name in an open folder (a phone still syncing, an export overwriting) revalidates and
    // re-parses; a plan is never served for bytes it did not come from. The mtime read is one
    // `metadata()` call against an 11 MB read, and a file whose metadata cannot be read is simply not
    // cached — the live path runs and answers for itself.
    //
    // L43 — WHAT UN-SETS IT. Three things, all of them wholesale: (a) a FOLDER SWAP, via
    // `note_folder_swap` above — the same chokepoint and the same generation that bound the decline
    // memo, so a plan can never outlive the folder that taught it; (b) the generation guard on READ,
    // which discards a map built under a previous generation even if a swap somehow raced the clear;
    // (c) the BYTE BUDGET below, which evicts oldest-first inside one folder. Nothing else clears it,
    // there is no timer, and no single entry can be invalidated by hand — the mtime key is what makes
    // hand-invalidation unnecessary.
    //
    // v0.8.160 (P3/U6) — AND A FOURTH RULE, ON THE WRITE SIDE: A PARSE FROM A DEAD FOLDER IS NOT
    // FILED. This block's own list was written as if the only risk were RESIDENCY, and it is not.
    // `cached_tile_source` reads the generation, then spends the read + parse, then files; a swap in
    // that window used to stamp the map with the ORPHAN'S generation, which (i) left the LIVE
    // folder's cache marked dead so every read missed and paid the re-read + re-parse §3 removed,
    // and (ii) let each further straggler WIPE what the new folder had banked and re-stamp it dead —
    // repeatedly, through the folder-open storm. `plan_cache_slot` re-reads the live generation
    // UNDER THIS LOCK and declines. SERVING correctness was never breached: `plan_cache_serves`
    // holds both its terms throughout.
    //
    // WHY A BYTE BUDGET AT ALL (the ruling says "plans are small" — a plan IS small, but a
    // `TileSource` is not). It carries `tiles: Vec<Vec<u8>>`, the file's compressed HEVC payloads,
    // which is nearly the whole 11 MB. Folder-generation alone would therefore let a 100-file phone
    // folder park ~1 GB of tile bytes beside an 8 GB L2 — a memory regression traded for a parse. The
    // budget keeps the WIN (the repeats, which are near in time: the same photo at two tiers seconds
    // apart, or a step back over a warm neighbourhood) and drops the tail nobody re-asks for.
    const PLAN_CACHE_BYTES: u64 = 128 * 1024 * 1024;

    /// One cached plan: the parse, the mtime it was parsed FROM, and what it costs to hold.
    /// `Arc` because a hit must not deep-copy the tile payloads — that would trade the read for a
    /// memcpy of the same size and prove nothing.
    struct CachedPlan {
        mtime: std::time::SystemTime,
        src: Arc<TileSource>,
        bytes: u64,
    }

    /// `(folder generation, entries, insertion order, bytes held)`. FIFO eviction, not LRU: the
    /// access pattern this exists for is a browse, where "oldest inserted" and "furthest from the
    /// user" are the same photograph, and a FIFO cannot be made to thrash by a repeated hit.
    #[allow(clippy::type_complexity)]
    static PLAN_CACHE: Mutex<
        Option<(u64, HashMap<PathBuf, CachedPlan>, std::collections::VecDeque<PathBuf>, u64)>,
    > = Mutex::new(None);

    /// What one parsed plan costs to keep: the tile payloads dominate, and the parameter sets and
    /// geometry are a rounding error beside them. Approximate on purpose — the budget is a bound, not
    /// an accountant.
    fn plan_bytes(tiles: &[Vec<u8>]) -> u64 {
        tiles.iter().map(|t| t.len() as u64).sum()
    }

    /// May a cached plan be served? The WHOLE of the cache's correctness, as one pure predicate.
    ///
    /// Two terms, and both are the ruling's own words. `entry_gen == gen_now` is the FOLDER-GENERATION
    /// bound (L43): a plan may never outlive the folder that taught it, and the read asks as well as
    /// the clear, so even a swap that raced the clear cannot serve a stale plan. `entry_mtime ==
    /// file_mtime` is the NAME-vs-BYTES rule (L42): the key is `(path, mtime)`, so a file rewritten
    /// under the same name is re-parsed rather than answered from the bytes it used to have.
    ///
    /// FALSIFIER (L28): drop the mtime term and
    /// `a_tile_plan_is_never_served_for_bytes_it_did_not_come_from` reddens on the rewritten-file
    /// row; drop the generation term and it reddens on the folder-swap row.
    #[inline]
    fn plan_cache_serves(
        entry_gen: u64,
        gen_now: u64,
        entry_mtime: std::time::SystemTime,
        file_mtime: std::time::SystemTime,
    ) -> bool {
        entry_gen == gen_now && entry_mtime == file_mtime
    }

    /// [`falcon_hwdec::tile_source`], memoised per `(path, mtime)` within the current folder.
    ///
    /// A miss parses and (if it fits the budget) files the result; a hit returns the same `Arc`. A
    /// file whose `metadata()` fails is parsed live and NOT filed — no mtime, no key, no entry.
    fn cached_tile_source(path: &Path) -> Result<Arc<TileSource>, HwDecError> {
        let gen = FOLDER_GEN.load(Ordering::Relaxed);
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(mtime) = mtime {
            let g = PLAN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((g_gen, map, _, _)) = g.as_ref() {
                if let Some(p) = map.get(path) {
                    if plan_cache_serves(*g_gen, gen, p.mtime, mtime) {
                        return Ok(p.src.clone());
                    }
                }
            }
        }
        let src = Arc::new(falcon_hwdec::tile_source(path)?);
        if let Some(mtime) = mtime {
            let bytes = plan_bytes(&src.tiles);
            if bytes <= PLAN_CACHE_BYTES {
                file_plan(gen, path, mtime, &src, bytes);
            }
        }
        Ok(src)
    }

    /// The cache's whole shape: `(folder generation, entries, insertion order, bytes held)`.
    #[allow(clippy::type_complexity)]
    type PlanSlot = (u64, HashMap<PathBuf, CachedPlan>, std::collections::VecDeque<PathBuf>, u64);

    /// v0.8.160 (P3/U6) — **MAY A PARSE THAT STARTED IN ANOTHER FOLDER BE FILED AT ALL?**
    ///
    /// `cached_tile_source` reads the generation, then spends ~11 MB of read and a full container
    /// parse, then files. A folder swap in that window makes the parse an ORPHAN — and the shipped
    /// code stamped the cache with the ORPHAN'S generation. Two harms, both about the LIVE folder
    /// rather than about residency: a filing that arrives before the new folder's own first filing
    /// leaves the map stamped dead (every later read misses and the §3 re-read+re-parse comes back),
    /// and a straggler arriving AFTER it wipes the new folder's live entries and re-stamps them
    /// dead — repeatedly, once per straggler, during exactly the folder-open storm the cache exists
    /// to flatten.
    ///
    /// So an orphan is not filed. The live generation is re-read UNDER THIS LOCK, which is what
    /// makes it a decision rather than a race: `note_folder_swap` bumps the counter BEFORE it takes
    /// this lock to clear, so a `parse_gen == live_gen` answer here means no clear can interleave
    /// between the read and the insert. The existing reset stays, keyed on the LIVE generation — it
    /// is the belt for a map that somehow survived a clear.
    fn plan_cache_slot(g: &mut Option<PlanSlot>, parse_gen: u64, live_gen: u64) -> Option<&mut PlanSlot> {
        if parse_gen != live_gen {
            return None;
        }
        let entry = g.get_or_insert_with(|| (live_gen, HashMap::new(), std::collections::VecDeque::new(), 0));
        if entry.0 != live_gen {
            *entry = (live_gen, HashMap::new(), std::collections::VecDeque::new(), 0);
        }
        Some(entry)
    }

    /// File one parsed plan against the CURRENT generation, evicting oldest-first until it fits.
    /// `gen` is the generation the PARSE started under — see [`plan_cache_slot`].
    fn file_plan(gen: u64, path: &Path, mtime: std::time::SystemTime, src: &Arc<TileSource>, bytes: u64) {
        let mut g = PLAN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let live = FOLDER_GEN.load(Ordering::Relaxed);
        let Some(entry) = plan_cache_slot(&mut g, gen, live) else { return };
        // A re-file of the same path (the mtime changed under us) replaces rather than doubles.
        if let Some(old) = entry.1.remove(path) {
            entry.3 = entry.3.saturating_sub(old.bytes);
            entry.2.retain(|p| p != path);
        }
        while entry.3 + bytes > PLAN_CACHE_BYTES {
            let Some(oldest) = entry.2.pop_front() else { break };
            if let Some(dropped) = entry.1.remove(&oldest) {
                entry.3 = entry.3.saturating_sub(dropped.bytes);
            }
        }
        entry.1.insert(path.to_path_buf(), CachedPlan { mtime, src: src.clone(), bytes });
        entry.2.push_back(path.to_path_buf());
        entry.3 += bytes;
    }

    /// The remembered post-work refusal that blocks THIS DOOR for this file in this folder, if
    /// there is one. See [`DeclineMemo::blocks`] for why the two doors get different answers.
    fn memoised_decline(path: &Path, out: FinishOut) -> Option<&'static str> {
        let gen = FOLDER_GEN.load(Ordering::Relaxed);
        // v0.8.187 (FRESH-1): the BYTES term. An entry filed against different bytes is not an
        // answer about this file — it is skipped, and the live path runs.
        let mtime = decline_mtime(path)?;
        let g = DECLINE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some((g_gen, map)) if *g_gen == gen => {
                map.get(path).filter(|(t, _)| *t == mtime).and_then(|(_, m)| m.blocks(out))
            }
            _ => None,
        }
    }

    /// Remember one post-work refusal against the CURRENT generation, under the SCOPE OF ITS FAULT
    /// — not under the door that happened to meet it. See [`DeclineMemo::note`] and the module
    /// block above for why those are different questions (v0.8.168, F4).
    fn memoise_decline(path: &Path, scope: FinishOut, reason: &'static str) {
        let gen = FOLDER_GEN.load(Ordering::Relaxed);
        // v0.8.187 (FRESH-1): a file we cannot stat is not filed — same rule as `PLAN_CACHE`'s.
        let Some(mtime) = decline_mtime(path) else { return };
        let mut g = DECLINE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
        let entry = g.get_or_insert_with(|| (gen, HashMap::new()));
        if entry.0 != gen {
            *entry = (gen, HashMap::new());
        }
        // A re-file after a rewrite REPLACES rather than merges: the old bytes' refusals are not
        // evidence about the new ones, so the second door's slot starts empty too.
        let slot = entry.1.entry(path.to_path_buf()).or_insert_with(|| (mtime, DeclineMemo::default()));
        if slot.0 != mtime {
            *slot = (mtime, DeclineMemo::default());
        }
        slot.1.note(scope, reason);
    }

    /// v0.8.148 (E5): probe the box's hardware HEIC capability ONCE, state the verdict in one line,
    /// and arm the lane if — and only if — every part of the chain answered.
    ///
    /// Runs on its own thread and is never joined, for the same two reasons `probe_heic_once` does:
    /// it must not delay a boot, and it builds a wgpu device, which is not something to do on the UI
    /// thread while Slint is still selecting a backend. It is called AFTER the backend selection has
    /// completed so the auto-tune's own timing probes are not measuring a box that is simultaneously
    /// creating another device.
    pub(crate) fn probe_and_arm() {
        static ONCE: AtomicBool = AtomicBool::new(false);
        if ONCE.swap(true, Ordering::Relaxed) {
            return;
        }
        std::thread::spawn(|| {
            // v0.8.165 (WAVE 1): every exit of the probe answers the SAME question — is the
            // hardware lane live? — so the COLOUR arm is stated once, from that one answer, at
            // whichever exit was taken. Six per-exit sentences would be six things that have to
            // agree, which is the U15 class this line exists to avoid.
            let live = probe_inner();
            log_colour_arm(live);
        });
    }

    /// The probe's body. `true` iff the lane was armed. Split out of the thread closure in v0.8.165
    /// so [`log_colour_arm`] has exactly one place to be called from.
    fn probe_inner() -> bool {
            let t0 = Instant::now();
            // Plan non-negotiable #5: the kill switch must FORCE WIC, and the strongest way to force
            // it is to never install the door. Checked here as well as in `decode_heic_lane` — two
            // locks on one switch, and this one also keeps the probe's cost off a classic boot.
            if falcon_decode::classic_heic() {
                log_event(
                    "heic hw: NOT PROBED (FALCON_CLASSIC_HEIC=1) — the hardware lane is not \
                     installed at all this session; every tier takes the v0.8.100 WIC path",
                );
                return false;
            }
            if hw_disabled_from_env(std::env::var("FALCON_HW_HEIC").ok().as_deref()) {
                log_event(
                    "heic hw: DISABLED (FALCON_HW_HEIC=0) — the probe was skipped and the lane is \
                     not installed; HEIC decodes exactly as v0.8.147 shipped it (S1 thumbs, S2 \
                     fast, full decode at native)",
                );
                return false;
            }
            // (1) THE VIDEO DEVICE. Asked of `ID3D11VideoDevice` itself and never of ffmpeg — Stage
            // 0 recorded a build whose `-hwaccel d3d11va` enumerated ZERO GUIDs on this very box and
            // then silently decoded in software, which is the whole reason it is asked here.
            let dev = match DecodeDevice::new() {
                Ok(d) => d,
                Err(e) => {
                    log_event(&format!(
                        "heic hw: NO — no D3D11 hardware device with video support on this box \
                         ({e}); HEIC decodes through WIC exactly as before, which is a supported \
                         configuration and not a degradation"
                    ));
                    return false;
                }
            };
            let profiles = dev.profile_count();
            if !dev.supports_hevc_main_nv12() {
                log_event(&format!(
                    "heic hw: NO — the video device advertises {profiles} decoder profile(s) but \
                     CheckVideoDecoderFormat(HEVC_VLD_MAIN, NV12) says no; HEIC decodes through WIC"
                ));
                return false;
            }
            // (2) THE ASSEMBLY DEVICE. Its own wgpu device, asking for the ADAPTER's limits.
            let asm = match HeicAssembler::headless() {
                Ok(a) => a,
                Err(e) => {
                    log_event(&format!(
                        "heic hw: NO — the video device decodes HEVC Main/NV12 but the GPU assembly \
                         would not build ({e}); HEIC decodes through WIC"
                    ));
                    return false;
                }
            };
            let lim = asm.device().limits();
            let store = u64::from(lim.max_storage_buffer_binding_size);
            log_event(&format!(
                "heic hw: YES — D3D11VA HEVC Main/NV12, {profiles} decoder profiles; assembly on \
                 '{}' (storage binding {store} B = mosaics to {:.0} MP, max texture {} px); \
                 {MAX_SESSIONS} session(s) × {SURFACES} surfaces; probed in {} ms. Thumbs keep the \
                 S1 embedded-preview door; fast and full-res route to hardware and fall soft to WIC \
                 per file. FALCON_CLASSIC_HEIC=1 forces WIC; FALCON_HW_HEIC=0 disables the probe.{}",
                asm.adapter_name(),
                (store / 4) as f64 / 1e6,
                lim.max_texture_dimension_2d,
                t0.elapsed().as_millis(),
                // v0.8.157 (§5): the lane cap's state, stated at boot beside the lane it caps, so a
                // log answers "which arm was this run?" without inference — the same rule the
                // `pool posture:` line follows. The unset case says so explicitly rather than
                // staying silent: "no line" and "the lever was 0" must not look alike in a
                // measurement campaign that A/Bs exactly those two arms.
                // v0.8.160 (U15/F4-#5): the lever is INERT under `FALCON_CLASSIC_POSTURE=1` — the
                // tick passes `None` there — so this line must not claim a cap that will never be
                // applied. Three states, three sentences; "set but inert" and "set and binding" are
                // exactly the pair a measurement campaign must not confuse.
                // v0.8.169 (item 7): …and it names the SOURCE now, because there are TWO. The env
                // lever is no longer the only way to move this number — Settings → Developer sets
                // it too, and `support::heic_lane_cap` resolves env-over-setting. So the line asks
                // `heic_lane_cap()` for the EFFECTIVE value (as it always did) and
                // `heic_lane_cap_env()` for WHICH source produced it. A run whose cap came from a
                // click and a run whose cap came from a shell export must not read alike: they
                // reproduce differently, and reproducing a measurement arm is what this line is for.
                // NOTE the ordering contract this depends on: main seeds the setting into
                // `heic_lane_cap` immediately after `load_settings`, ABOVE `probe_and_arm`.
                match (support::classic_posture(), support::heic_lane_cap()) {
                    (true, Some(n)) => format!(
                        " lane cap {n} ({}) is set but INERT: FALCON_CLASSIC_POSTURE=1 is \
                         also set, and the classic revert passes no lane cap at all.",
                        lane_cap_source()
                    ),
                    (true, None) => " no lane cap is set (and one would be inert anyway \
                                      under FALCON_CLASSIC_POSTURE=1)."
                        .to_string(),
                    (false, Some(n)) => format!(
                        " lane cap {n} IS SET, from {} — at most {n} speculative HEIC decodes \
                         may be in flight on a folder this lane has promoted to CHEAP, and ONLY on \
                         such a folder (v0.8.160: a folder the lane declines keeps the shipped \
                         costly cap). The displayed shot is exempt, so the ceiling is {}; the CHEAP \
                         runway is untouched.",
                        lane_cap_source(),
                        n.saturating_add(1)
                    ),
                    // v0.8.171 (OWNER RULING): the sentence that called uncapped "this build's
                    // default" DIES HERE — the default is 4 now, and reaching this arm means
                    // somebody chose otherwise. It cannot be reached by silence any more: an
                    // install with no stored choice resolves to 4 through
                    // `support::heic_lane_cap_from_settings` before this line runs.
                    (false, None) => " no lane cap is set — FALCON_HEIC_LANE_CAP is unset AND \
                             Settings → Developer has been put at Uncapped EXPLICITLY, so \
                             speculative concurrency on a promoted (CHEAP) folder is UNCAPPED. \
                             This build's DEFAULT is 4 (the 08-06 laptop pair: 30 five-second \
                             give-ups to WIC uncapped, against 4 at cap 4)."
                        .to_string(),
                },
            ));
            arm(asm);
            ASSEMBLY_STORAGE_LIMIT.store(store, Ordering::Relaxed);
            maybe_log_topology();
            true
    }

    /// v0.8.169 (item 7): WHICH of the two sources produced the effective lane cap. One expression,
    /// used by both arms of the boot line above, so the two can never disagree about provenance.
    fn lane_cap_source() -> &'static str {
        if support::heic_lane_cap_env().is_some() {
            "the FALCON_HEIC_LANE_CAP env lever (which overrides the setting for this whole run)"
        } else {
            "the Settings → Developer control"
        }
    }

    /// v0.8.165 (WAVE 1) — **WHICH COLOUR ARM SERVES THIS SESSION'S HEIC FULL-RES FRAMES**, said
    /// once at boot, in every configuration, so a measurement run never has to infer it.
    ///
    /// The U15 discipline: "the lever is unset", "the lever is 0" and "the lever is on but there is
    /// no lane" are three different worlds and they must not look alike in a log. `CLASSIC_HEIC`
    /// and a failed capability probe both land in the third; `CLASSIC_POSTURE` does not appear
    /// because it governs the POOL, not the colour, and claiming otherwise would be the same
    /// over-reach `heic_lane_cap`'s boot clause was corrected for in v0.8.160.
    fn log_colour_arm(lane: bool) {
        let armed = gpu_color_armed();
        log_event(match (lane, armed) {
            // v0.8.167 (audit): AVAILABLE, not "are". This line is printed once, at boot, before
            // any file has been opened — and the arm is asked per file, behind gates that can and
            // do say no (the renderer's texture limit, the folder's service state, a mosaic, a
            // saturated pool). The old wording promised that the lane's full-res frames ARE
            // colour-managed on the GPU, which a run full of `hw-heic-decline-render-limit` notes
            // would contradict on its own evidence. What a boot can honestly state is which arm is
            // available to be tried; the per-frame answer is the `gpu-color` / `cpu-color` marker
            // on each `full-res #N:` line, and that is where a measurement pass must read it.
            (true, true) => concat!(
                "heic colour: GPU AVAILABLE — a hardware-lane HEIC full-res frame can be ",
                "colour-managed on the ASSEMBLY device, inside the finish pass that already crops, ",
                "rotates and resamples it, arriving at the renderer as display-ready RGBA with the ",
                "CPU transform and the RGB->RGBA expansion off its path. It is asked per FILE and ",
                "can decline (see hw-heic-decline-*); the `gpu-color` / `cpu-color` marker on each ",
                "full-res line is the per-frame answer. FALCON_HEIC_GPU_COLOR=0 reverts to the CPU ",
                "arm.",
            ),
            (true, false) => concat!(
                "heic colour: CPU (FALCON_HEIC_GPU_COLOR=0) — the hardware lane is live and ",
                "serving, but its frames come back in the file's own gamut and ",
                "falcon_color::transform_rgba converts them on the decode worker, exactly as ",
                "v0.8.164 shipped it.",
            ),
            (false, _) => concat!(
                "heic colour: CPU — there is no hardware lane this session, so every HEIC is a ",
                "WIC decode in the file's own gamut and falcon_color::transform_rgba converts it. ",
                "The GPU colour arm has nothing to attach to (it rides the assembly device, which ",
                "only the lane builds).",
            ),
        });
    }

    /// v0.8.148 (E5) — THE TOPOLOGY VERDICT, as a pure function of the two measured limits, so the
    /// sentence a log carries is testable without a GPU.
    ///
    /// The choice is: does the grid assembly run on the app's RENDER device (shared) or on one of its
    /// own (dedicated)? The decode device was never in question — M1 shipped it dedicated and SP3
    /// measured the cost of that at +0.11 ms of renderer frame time. The assembly is the open half,
    /// and it is decided here by the only fact that can decide it: whether the renderer's device
    /// could hold the mosaic at all.
    pub(crate) fn topology_verdict(assembly_store: u64, render_store: Option<u64>) -> String {
        let Some(render_store) = render_store else {
            return "assembly on its OWN wgpu device (the renderer's limits had not been captured \
                    when the probe ran, so this line states the choice without its measurement)"
                .to_string();
        };
        if render_store < MOSAIC_48MP_BYTES {
            let none = if render_store == 0 {
                " (NO storage buffer at all, which is what a downlevel-compatible device descriptor \
                 buys)"
            } else {
                ""
            };
            format!(
                "assembly on its OWN wgpu device — DECIDED BY MEASUREMENT: the renderer's device \
                 admits {render_store} B per storage binding{none}, a 48 MP photo's mosaic needs \
                 {MOSAIC_48MP_BYTES} B, so a shared device could not hold the owner's own corpus; \
                 this one admits {assembly_store} B"
            )
        } else {
            format!(
                "assembly on its OWN wgpu device — the renderer's device WOULD hold the mosaic \
                 ({render_store} B against the 48 MP mosaic's {MOSAIC_48MP_BYTES} B), so here the \
                 split buys isolation rather than capability: the assembly's compute submissions do \
                 not queue behind UI frames and a lost assembly device is not a lost renderer"
            )
        }
    }

    /// Publish the assembler, latch the lane live, install the door — in that order, so no thread can
    /// observe a live lane with no assembler behind it.
    fn arm(asm: HeicAssembler) {
        if ASSEMBLER.set(Arc::new(asm)).is_err() {
            return; // already armed; `ONCE` makes this unreachable, and it is still not a panic
        }
        LANE_LIVE.store(true, Ordering::Relaxed);
        if !falcon_decode::install_hw_heic_hook(hw_decode_heic) {
            log_event("heic hw: a decode hook was already installed — leaving the first in place");
        }
        // v0.8.149 (F1) — WHAT THIS LINE USED TO SAY, AND WHY IT NO LONGER SAYS IT.
        //
        // v0.8.148 printed "HEIC reclassified CHEAP — the hardware lane is live" HERE, at arm time,
        // and that sentence contained the round's one RED. It reclassified the format for the whole
        // session on the strength of a CAPABILITY probe, and then admitted the cost in its own last
        // clause: "a file that DECLINES the lane still decodes through WIC and is still counted
        // cheap — that is the bound this posture accepts". The E6 audit measured that bound and it
        // is not one a user would accept: on an ordinary folder of single-item HEICs (Windows,
        // Android, Adobe — everything that is not a phone's own grid) the lane declines every file,
        // so the folder ran WIC's seconds-per-frame decode with the flood protections switched off,
        // 4.3x worse on the fast median and 5.4-8.5x worse on the visible frame than the SAME
        // BINARY with the lane disabled.
        //
        // So arming the lane is no longer a reclassification, and this line says what it actually
        // is: the lane is available, and whether it is USED is now a per-folder fact that the folder
        // itself decides. The reclassification lines come later, from `support::hw_service_line`,
        // once a folder has earned one — and they can go the other way too.
        if !support::classic_posture() {
            log_event(
                "pool posture: the hardware HEIC lane is AVAILABLE, and the costly-format \
                 machinery stays ARMED until a folder proves it is being served. A live lane is not \
                 a serving lane: every single-item (non-grid) HEIC declines this lane, so a folder \
                 of Windows/Android/Adobe output would otherwise pay WIC's cost with WIC's \
                 protections switched off. Each folder therefore opens COSTLY, stands down for HEIC \
                 after 2 hardware-served browse decodes in that folder (`pool posture: HEIC \
                 reclassified CHEAP`), and re-arms if declines then dominate. Every transition is \
                 logged; nothing here is inferred from a quiet pool",
            );
        }
    }

    static ASSEMBLER: OnceLock<Arc<HeicAssembler>> = OnceLock::new();

    // ── the session pool ─────────────────────────────────────────────────────────────────────────

    /// A built session and the tile geometry it was built for. A [`PhotoDecoder`] is per TILE
    /// GEOMETRY — a 48 MP file's 896×1024 session cannot decode a 24 MP file's 640×896 tiles and says
    /// so rather than trying — so the pool is keyed on that pair and a mixed folder holds two.
    struct Pooled {
        key: (u32, u32),
        dec: PhotoDecoder,
    }

    #[derive(Default)]
    struct PoolState {
        /// Built and free.
        idle: Vec<Pooled>,
        /// Built at all — `idle.len() + out`.
        live: usize,
        /// Checked out right now.
        out: usize,
    }

    static POOL: Mutex<Option<PoolState>> = Mutex::new(None);
    static POOL_CV: Condvar = Condvar::new();

    // ── v0.8.149 (F9/A10 + F6): what the pool actually did, counted ─────────────────────────────
    //
    // A10: a `PhotoDecoder` is per TILE GEOMETRY, and a mixed folder (12 MP beside 48 MP) makes the
    // pool RETIRE an idle session of one shape to build one of another. That costs a
    // `CreateVideoDecoder` — Stage 0 measured 2–5.5 ms — plus the surface array, and v0.8.148 had no
    // way to see it happening at all: a folder thrashing two geometries and a folder holding two
    // steady produced identical logs. Two counters and a flush line make the churn a number.
    //
    // F6: …and the WAIT distribution, which is the evidence the next round should re-derive
    // `HW_WAIT_MS` from rather than re-deriving it from a comment (as v0.8.148's did).
    static SESSIONS_CREATED: AtomicUsize = AtomicUsize::new(0);
    static SESSIONS_RETIRED: AtomicUsize = AtomicUsize::new(0);
    /// Checkouts that had to wait at all, and the worst wait seen, in ms.
    static WAITED: AtomicUsize = AtomicUsize::new(0);
    static WAIT_MAX_MS: AtomicU64 = AtomicU64::new(0);
    /// …and how many gave up ([`HW_WAIT_MS`] expired) — the `busy` declines.
    static WAIT_EXPIRED: AtomicUsize = AtomicUsize::new(0);

    /// v0.8.149 (F9/A10 + F6): the pool's own line, on `flush_decode_stats`'s ~1.5 s cadence, and
    /// ONLY when something has moved. An idle session, a JPEG folder and a box with no lane all stay
    /// silent, so this cannot become the per-decode spam this codebase keeps not shipping.
    pub(crate) fn session_churn_report() -> Option<String> {
        static LAST: Mutex<(usize, usize, usize, usize)> = Mutex::new((0, 0, 0, 0));
        let now = (
            SESSIONS_CREATED.load(Ordering::Relaxed),
            SESSIONS_RETIRED.load(Ordering::Relaxed),
            WAITED.load(Ordering::Relaxed),
            WAIT_EXPIRED.load(Ordering::Relaxed),
        );
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == now {
            return None;
        }
        *last = now;
        let (created, retired, waited, expired) = now;
        // `created - retired` rather than reading `PoolState::live` under the pool lock. This runs on
        // the UI thread, inside the tick's `plog+hud` step, and the pool lock is held by eighteen
        // workers during a flood — including across a session DROP, which is a COM release of a
        // decoder plus its surface array. A 51 ms `plog+hud` tick was measured on the first build
        // that took the lock here, and paying a tick for an observability line is exactly the trade
        // this codebase does not make. Two relaxed loads that were going to be read anyway give the
        // same number.
        let live = created.saturating_sub(retired);
        Some(format!(
            "heic sessions: {created} created, {retired} retired (geometry churn), {live} live of \
             {MAX_SESSIONS}; {waited} checkout(s) waited, worst {} ms, {expired} gave up at \
             {HW_WAIT_MS} ms and fell soft to WIC",
            WAIT_MAX_MS.load(Ordering::Relaxed),
        ))
    }

    /// How many sessions this LANE may hold at once.
    ///
    /// The interactive tier gets the whole pool; the speculative one gets all but one slot. That
    /// reservation is the entire reason this is a function: the fast pool is 18 workers wide and will
    /// happily ask for every slot there is, and the shot the user is LOOKING AT must not queue behind
    /// a speculative prefetch of a shot he may never reach. It costs the prefetcher one slot's
    /// throughput, and it is the same priority rule the costly-decode cap already encodes elsewhere.
    ///
    /// FALSIFIER (L28): return `max` for both arms and `the_detail_tier_keeps_a_session_slot` reddens.
    #[inline]
    pub(crate) fn session_budget(lane: Lane, max: usize) -> usize {
        match lane {
            Lane::Native => max,
            _ => max.saturating_sub(1).max(1),
        }
    }

    /// Take a session for `src`'s tile geometry, building one if the pool has room, retiring an idle
    /// session of another geometry if it does not, and waiting — bounded — if every slot is out.
    ///
    /// `None` means "decline this file": either the build failed (the reason is noted) or the wait
    /// expired. Never an error and never a block without a bound.
    fn checkout(src: &TileSource, lane: Lane, asm: &Arc<HeicAssembler>) -> Option<Lease> {
        let key = (src.tile_w, src.tile_h);
        let budget = session_budget(lane, MAX_SESSIONS);
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_millis(HW_WAIT_MS);
        let mut waited = false;
        let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let mut make = false;
            {
                let st = g.get_or_insert_with(PoolState::default);
                if st.out < budget {
                    if let Some(i) = st.idle.iter().position(|p| p.key == key) {
                        let p = st.idle.swap_remove(i);
                        st.out += 1;
                        drop(g);
                        note_wait(waited, t0);
                        return Some(Lease::new(p));
                    }
                    // No session of this geometry is free. Retire an idle one of ANOTHER geometry to
                    // make room rather than sit behind it — a folder mixing a 12 MP and a 48 MP
                    // camera would otherwise starve one of the two shapes forever. The drop happens
                    // under the lock; it is a COM release of an object this thread has just proved
                    // nobody else holds, and it is rare (only on geometry churn).
                    if st.live >= MAX_SESSIONS {
                        if let Some(old) = st.idle.pop() {
                            st.live -= 1;
                            SESSIONS_RETIRED.fetch_add(1, Ordering::Relaxed);
                            drop(old);
                        }
                    }
                    if st.live < MAX_SESSIONS {
                        st.live += 1;
                        st.out += 1;
                        make = true;
                    }
                }
            }
            if make {
                drop(g);
                note_wait(waited, t0);
                return match PhotoDecoder::with_assembler(src, SURFACES, asm.clone()) {
                    Ok(dec) => {
                        SESSIONS_CREATED.fetch_add(1, Ordering::Relaxed);
                        Some(Lease::new(Pooled { key, dec }))
                    }
                    Err(e) => {
                        release_slot(false);
                        decline(
                            "session",
                            format!("no decoder session for {}x{} tiles ({e})", key.0, key.1),
                        );
                        None
                    }
                };
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Never note under the pool lock — the same rule `note_decode` follows for the
                // stats lock. There is no cycle here (nothing takes the note channel then the pool),
                // but a lock held across an allocation and a `HashSet` insert is a habit worth not
                // forming.
                drop(g);
                WAIT_EXPIRED.fetch_add(1, Ordering::Relaxed);
                note_wait(true, t0);
                decline(
                    "busy",
                    format!(
                        "all {MAX_SESSIONS} hardware decode sessions were in use for {HW_WAIT_MS} ms"
                    ),
                );
                return None;
            }
            waited = true;
            g = POOL_CV.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// v0.8.149 (F6): fold one checkout's wait into the distribution [`session_churn_report`] prints.
    /// Two relaxed atomics on a path that has just done (or is about to do) tens of ms of work.
    fn note_wait(waited: bool, t0: Instant) {
        if !waited {
            return;
        }
        WAITED.fetch_add(1, Ordering::Relaxed);
        let ms = t0.elapsed().as_millis() as u64;
        WAIT_MAX_MS.fetch_max(ms, Ordering::Relaxed);
    }

    /// Give a checked-out slot back. `keep_live` false also retires the session's existence — used
    /// when the build failed (there is nothing to keep) and when a decode failed.
    fn release_slot(keep_live: bool) {
        {
            let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
            let st = g.get_or_insert_with(PoolState::default);
            st.out = st.out.saturating_sub(1);
            if !keep_live {
                st.live = st.live.saturating_sub(1);
            }
        }
        POOL_CV.notify_one();
    }

    /// v0.8.149 (F2) — **A CHECKED-OUT SESSION THAT CANNOT LEAK ITS SLOT.**
    ///
    /// # The leak this closes
    ///
    /// v0.8.148 returned a bare `Pooled` and called `checkin` on each of the two normal paths. Every
    /// path that is NOT a normal return therefore leaked `PoolState::out` — permanently, because
    /// nothing ever decrements it again. The reachable one is a panic: the decode runs inside the
    /// fast pool's `catch_unwind` (and the detail worker's), which is there precisely because a
    /// third-party codec on a crafted or corrupt file must not kill the worker — so the app SURVIVES
    /// the panic and carries on with one slot gone. Two of those and the speculative tier's budget
    /// ([`session_budget`] = MAX-1 = 2) is exhausted; three and the whole lane is: every subsequent
    /// HEIC then waits the full [`HW_WAIT_MS`] and falls soft, so one crafted file quietly converts
    /// the epic into a slower version of v0.8.147 with a per-file stall bolted on.
    ///
    /// # Why a guard rather than a `catch_unwind` here
    ///
    /// Catching the panic in the router would swallow it — the worker's own handler is what decides
    /// what a failed decode means, and it already does the right thing. A `Drop` guard changes
    /// nothing about who handles the panic and only makes the accounting true on the way past, which
    /// is what a slot is: a resource, released by scope, like every other RAII in this codebase.
    ///
    /// # It is also a DECLINE-class event (F1)
    ///
    /// An unwound decode never reaches `note_decode`, so without this the file would be invisible to
    /// the service ledger — a folder that panics its way through the lane would look like a folder
    /// with no attempts rather than one being served by nobody. The `Drop` records it as a decline
    /// on exactly the same footing as a `container` or a `vui` refusal, because from the browse's
    /// point of view that is what it was: this file did not come off the hardware.
    ///
    /// FALSIFIER: `FALCON_HW_HEIC_PANIC=<substring>` panics one named file's decode after checkout.
    /// Without this guard, three matching files kill the lane for the session; with it, the NEXT
    /// file still routes to hardware and the log says so.
    struct Lease {
        inner: Option<Pooled>,
        /// Set by [`Lease::settle`] on every normal return. `false` in `Drop` means UNWIND.
        settled: bool,
        /// Whether the session itself survives. False after any decode error: a `DecodeSession`
        /// latches `DeviceLost` and refuses everything afterwards, so a session that has failed once
        /// must not be handed to the next file — that is how one bad frame becomes a folder of them.
        /// Also false on an unwind, where the session's state is simply unknown.
        keep: bool,
    }

    impl Lease {
        fn new(p: Pooled) -> Lease {
            Lease { inner: Some(p), settled: false, keep: false }
        }
        fn dec(&mut self) -> &mut PhotoDecoder {
            // Infallible: `inner` is only taken in `Drop`, and `Drop` consumes the Lease.
            &mut self.inner.as_mut().expect("a Lease holds its session until it is dropped").dec
        }
        /// The normal return. `keep` returns the session to the pool; `!keep` retires it.
        fn settle(mut self, keep: bool) {
            self.settled = true;
            self.keep = keep;
        }
    }

    impl Drop for Lease {
        fn drop(&mut self) {
            let p = self.inner.take();
            let keep = self.settled && self.keep;
            {
                let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
                let st = g.get_or_insert_with(PoolState::default);
                st.out = st.out.saturating_sub(1);
                match (keep, p) {
                    (true, Some(p)) => st.idle.push(p),
                    (_, p) => {
                        st.live = st.live.saturating_sub(1);
                        SESSIONS_RETIRED.fetch_add(1, Ordering::Relaxed);
                        drop(p);
                    }
                }
            }
            POOL_CV.notify_one();
            if !self.settled {
                // UNWIND. Never under the pool lock (`decline` allocates and takes the note
                // channel), and never a panic of its own: `note_decode_once` recovers a poisoned
                // mutex rather than unwrapping, which matters here because this runs DURING an
                // unwind and a second panic would abort the process.
                decline(
                    "panic",
                    "the decode panicked; the session slot was released on unwind and the session \
                     retired"
                        .to_string(),
                );
                support::note_hw_heic_route(SrcKind::Heic, DecodeRoute::Cpu);
            }
        }
    }

    // ── the router ───────────────────────────────────────────────────────────────────────────────

    fn decline(reason: &'static str, why: String) {
        note_decode_once(
            &format!("hw-heic-decline-{reason}"),
            format!(
                "heic hw: this file declined the hardware lane ({why}) — it decoded through WIC \
                 instead. Per FILE, not a session-wide fallback: the next HEIC still tries hardware."
            ),
        );
    }

    /// v0.8.167 (audit) — the COLOUR door's decline, which is a different sentence.
    ///
    /// [`decline`] says "it decoded through WIC instead", and for the hook's refusals that is
    /// true: the hook IS rung 0 of the WIC ladder, so a `None` there drops the file to rung 1.
    /// The MANAGED door's refusals are not that. A `None` from it falls through to the ordinary
    /// detail chain, which still contains the hardware lane through the hook — so the file
    /// decodes on the GPU's video engine exactly as before and only its COLOUR moves back to
    /// `falcon_color::transform_rgba` on the decode worker. Printing "it decoded through WIC"
    /// there would send a field reader hunting a software decode that never happened (L30: a log
    /// must name the outcome, and this one was naming the wrong path).
    ///
    /// v0.8.168 (F4) — WHICH SENTENCE GOES WHERE, as a rule rather than a habit: the wording
    /// follows the BLOCK SCOPE the refusal produces. This one is for the managed door's two
    /// PRE-WORK render-limit refusals, which memoise nothing at all (they are arithmetic, and the
    /// memo remembers only refusals that spent the hardware budget) — so the file really does keep
    /// the lane and really does only lose its colour arm. The two POST-WORK refusals take
    /// [`decline`]'s WIC wording on either door, because since v0.8.168 they block BOTH doors: the
    /// hook declines next, and rung 1 of the ladder is WIC. Neither sentence is a guess about the
    /// outcome; each is the outcome its own call site produces.
    fn decline_colour(reason: &'static str, why: String) {
        note_decode_once(
            &format!("hw-heic-decline-{reason}"),
            format!(
                "heic hw: this file declined the GPU COLOUR arm ({why}) — it still decodes on the \
                 hardware lane; only its colour transform runs on the CPU, exactly as v0.8.164 \
                 shipped it. Per FILE: the next HEIC still tries the GPU colour arm."
            ),
        );
    }

    /// **THE HOOK.** Installed into `falcon_decode` at boot; called from `decode_heic_lane`'s rung 0
    /// on the fast pool's workers and on the detail tier's own worker — the SAME lanes that own the
    /// equivalent WIC decode today, so the pool's accounting sees hardware work as work and no new
    /// thread exists to hide it from the tick's ACTIVE terms.
    ///
    /// The order of operations is E3-M2's published one, and the cheap checks come first on purpose:
    /// a file that cannot take this lane must find that out WITHOUT having taken a decoder session
    /// out of a pool of three.
    fn hw_decode_heic(path: &Path, scale_to: Option<u32>, lane: Lane) -> HwHeicAnswer {
        route(path, scale_to, lane, FinishOut::SourceRgb)
    }

    /// v0.8.165 (WAVE 1) — **THE DETAIL TIER'S GPU-COLOUR DOOR**, and it is a door rather than a
    /// mode on the hook for one reason: the hook's contract is the WIC ladder's contract (packed
    /// RGB8 in the file's own gamut, because rung 0 must be substitutable for rungs 1–3), and a
    /// frame that is already RGBA in the OUTPUT gamut is not substitutable for anything.
    ///
    /// So it sits exactly where `nv.decode_full_yuv` sits in the same worker: tried first, `None`
    /// falls through to the identical chain that ran before, and the chain still contains the
    /// hardware lane (through the hook) followed by the WIC ladder. That is the fail-soft, and it
    /// is structural — there is no state to unwind and no second code path to keep in step.
    ///
    /// `src`/`dst` are the DETAIL WORKER'S OWN gamut answers (`shot_source_gamut` and the live
    /// output gamut), passed in rather than re-derived here: one probe, one answer, no second
    /// reading of the file to drift from the first (L42).
    /// v0.8.172 (G2) — **AND ITS ANSWER HAS THREE VALUES**, because v0.8.171's collapse of one of
    /// them into `None` was this round's second R-severity defect.
    ///
    /// The reasoning it shipped with was that the caller's fall-through re-enters the chain, which
    /// contains the hook, which hears the same signal and abandons there too — "once instead of
    /// twice". What the fall-through actually costs is written out at [`support::managed_answer`],
    /// and every term of it was already measured by this round's own evidence base: a second
    /// `route()` walk whose checkout can block `HW_WAIT_MS`, a busy-decline into WIC's software HEVC
    /// decoder, and a DECLINE booked against a folder that is being served perfectly.
    pub(crate) fn decode_full_managed(
        path: &Path,
        scale_to: Option<u32>,
        src: falcon_color::Gamut,
        dst: falcon_color::Gamut,
    ) -> support::ManagedAnswer {
        if !gpu_color_armed() {
            return support::ManagedAnswer::Declined;
        }
        support::managed_answer(route(path, scale_to, Lane::Native, FinishOut::ManagedRgba { src, dst }))
    }

    /// v0.8.172 (G6) — **WOULD THIS FILE TAKE THE LANE AT THIS SIZE?** The one-decode gate's per-file
    /// pre-check, and it is [`route`]'s own gates rather than a description of them.
    ///
    /// `heic_fast_accelerated()` says the FOLDER is being served. Inside such a folder individual
    /// files still decline — a remembered post-work refusal, a mosaic past the assembly device's
    /// limits, a VUI E2's kernel refuses to guess at, a Main10 primary — and for those the enlarged
    /// one-decode ask is not a hardware decode at a bigger size, it is WIC being asked for a bigger
    /// frame than the scrub tier ever needed. That is the E6 audit's regression class, per file.
    ///
    /// COST: everything it runs is arithmetic over a container parse that
    /// [`cached_tile_source`] already holds for the folder, so within a folder this is the second and
    /// later files' cache hit plus four cheap validations.
    ///
    /// v0.8.173 (H4) — THE ONE FILE FOR WHICH THAT WAS NOT TRUE, corrected. v0.8.172 claimed the
    /// pre-check "is not a new cost" because the decode would have paid the same parse a moment
    /// later. That holds for a file the lane ADMITS. For one it refuses at parse time it did not:
    /// the three parse-time refusals were not memoised, so this gate read and parsed ~11 MB to say
    /// no, and the `route()` behind it read and parsed the same ~11 MB to say no again — per tier,
    /// per visit, on exactly the folders whose files are unparseable. Those three refusals are
    /// remembered now (see the `DECLINE_MEMO` block), so a refused file costs one parse per folder
    /// and this sentence is true as written.
    ///
    /// WHAT IT CANNOT ANSWER: the BUSY decline. Session availability is a fact about the instant the
    /// decode starts and no pre-check can know it. That file pays the enlarged WIC ask; the residual
    /// is bounded by the lane cap and is visible in the `busy` decline note and the
    /// `decode-stats fast HEIC/wic` row.
    pub(crate) fn lane_admits(path: &Path, scale_to: Option<u32>) -> bool {
        let Some(asm) = ASSEMBLER.get() else { return false };
        lane_live()
            && falcon_decode::hw_heic_lane_applies(Lane::Fast)
            && pre_checkout_gates(path, scale_to, FinishOut::SourceRgb, asm).is_some()
    }

    /// v0.8.172 (G6): [`route`]'s steps (0)–(4) — every refusal that can be known BEFORE a decoder
    /// session is taken out of a pool of three. Lifted so [`lane_admits`] runs the gates themselves
    /// rather than a second copy of them that could drift (L42), and so the decline notes those gates
    /// park stay in one place.
    ///
    /// `Some((src, want))` = this file reaches the checkout. `None` = it declines, and the reason has
    /// already been noted by whichever gate said so.
    fn pre_checkout_gates(
        path: &Path,
        scale_to: Option<u32>,
        out: FinishOut,
        asm: &Arc<HeicAssembler>,
    ) -> Option<(Arc<TileSource>, (u32, u32))> {
        // (0) THE INJECTED DECLINE, when a run has armed one. First, so it exercises the earliest
        // decline path there is — before the container is even read — which is what makes it a
        // faithful stand-in for "this file cannot take the lane".
        if let Some(pat) = injected_decline_pattern() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if hw_decline_injected(name, Some(pat)) {
                decline("injected", format!("FALCON_HW_HEIC_DECLINE matched '{pat}'"));
                return None;
            }
        }
        // (0b) v0.8.149 (F8): THE MEMO. A file that already spent the whole hardware budget in this
        // folder and came back with nothing does not spend it again on the next tier or the next
        // visit. Ahead of the container read, because that read is the first thing the memo saves.
        if let Some(reason) = memoised_decline(path, out) {
            let _ = reason; // the note for it was parked the first time; a second is not news
            return None;
        }
        // (1) THE CONTAINER — E1, once. Everything downstream reads this; nothing re-parses.
        // v0.8.157 (§3): …and, within one folder, nothing re-READS it either — see
        // [`cached_tile_source`].
        //
        // ── v0.8.173 (H4): A PARSE-TIME REFUSAL IS REMEMBERED NOW, AND THE OLD RULE'S PREMISE IS
        // WHAT CHANGED ──────────────────────────────────────────────────────────────────────────
        //
        // v0.8.157 left these three uncached on the reasoning that "a file that cannot be parsed is
        // re-parsed", and while the only caller was a decode that was cheap enough: the re-parse was
        // the price of a decode that was about to happen anyway. v0.8.172 (G6) added a SECOND caller
        // that is not a decode at all — `lane_admits`, the one-decode gate's per-file pre-check —
        // so an unparseable file in a serving folder now pays its ~11 MB read and full container
        // parse TWICE per ask (once to be refused by the gate, once to be refused by the router) and
        // again on every tier and every visit. That is the E6 flood class, arrived at from the other
        // direction.
        //
        // They are memoisable for the same reason the post-work refusals are: the verdict is
        // deterministic in the FILE, and the memo is bounded by the folder generation, so a folder
        // whose files have been replaced re-asks the hardware. (`plan_cache_serves` keys on mtime as
        // well and re-parses a rewritten file; this memo, like every other decline, keys on the
        // generation alone — a file rewritten under the same name WITHIN one folder open keeps its
        // refusal until the folder is re-scanned. That is the existing memo's contract, not a new
        // one, and the refusal it holds is "the bytes I read would not parse".)
        let src = match cached_tile_source(path) {
            Ok(s) => s,
            Err(e) => {
                // v0.8.149 (F7): the bit-depth refusal gets its OWN key, not `container`'s. It is
                // the one decline that is a statement about the PICTURE rather than about the file
                // being readable, and it is the one whose absence would have been silent — an 8-bit
                // `HEVC_VLD_MAIN` session fed a Main10 bitstream returns a wrong photo, not an
                // error. A field log has to be able to say "this folder is 10-bit" in one grep.
                let reason =
                    if matches!(e, HwDecError::BitDepth { .. }) { "bitdepth" } else { "container" };
                decline(reason, e.to_string());
                // v0.8.173 (H4): SourceRgb scope — the parse is the half both doors share, so this
                // blocks both, exactly as `dims` and `decode` do.
                memoise_decline(path, FinishOut::SourceRgb, reason);
                return None;
            }
        };
        // (2) THE DIMS THE CONTRACT PROMISES, before any routing decision — E3-M2's stated door for
        // exactly this caller. It is also the cheapest full validation of the geometry there is: it
        // re-derives `irot`, the crop and the mosaic and refuses a file whose display dims disagree
        // with E1's, all without touching the driver.
        let want = match photo::output_dims(&src, scale_to) {
            Ok(d) => d,
            Err(e) => {
                decline("geometry", e.to_string());
                // v0.8.173 (H4): remembered with the parse refusals above and for the same reason —
                // an `irot` that is not a quarter turn, or display dims that disagree with E1's, is
                // a fact about the FILE that will not become untrue on the next ask. It is derived
                // from the parsed source, so the memo also saves the parse that produced it.
                memoise_decline(path, FinishOut::SourceRgb, "geometry");
                return None;
            }
        };
        // (3) THE MOSAIC — will this device hold it? The honest pre-check, whose error names the
        // limit that said no.
        let fits = photo::geometry(&src)
            .map_err(|e| e.to_string())
            .and_then(|g| asm.mosaic_fits(&g).map_err(|e| e.to_string()));
        if let Err(e) = fits {
            decline("mosaic", e);
            return None;
        }
        // (3b) v0.8.165 (iGPU deliverable 2) — AND WILL THE RENDERER HOLD THE RESULT? Only the
        // managed door needs asking: its frame goes straight into a renderer texture, whereas the
        // source-gamut contract's frame passes through `finish_source`/`resize_to_long` and the
        // tier's own sizing on the way. Asked BEFORE a session is taken, so a device that cannot
        // take the frame costs the arithmetic and not the whole hardware budget. The limit is the
        // renderer's OWN reported number (`RENDER_MAX_TEX`), never a constant — a 16384-capped
        // iGPU and a 32768 discrete card must not share a hard-coded answer.
        if matches!(out, FinishOut::ManagedRgba { .. }) {
            let lim = RENDER_MAX_TEX.load(Ordering::Relaxed);
            match renderer_holds(want.0, want.1, lim) {
                Some(true) => {}
                Some(false) => {
                    decline_colour(
                        "render-limit",
                        format!(
                            "a {}x{} frame is past the RENDERER device's max texture dimension \
                             {lim}; the CPU colour path resizes on the way instead",
                            want.0, want.1
                        ),
                    );
                    return None;
                }
                None => {
                    decline_colour(
                        "render-unmeasured",
                        "the renderer's texture limit has not been captured yet (no frame has \
                         been drawn); this file takes the CPU colour path"
                            .to_string(),
                    );
                    return None;
                }
            }
        }
        // (4) THE COLOUR PARAMETERS — E2's kernel refuses what it does not model, including
        // `matrix_coeffs` 2 (UNSPECIFIED), which is not a licence to pick one. A file that lands here
        // belongs to WIC, and saying so is the L42 discipline: the answering bytes are the VUI's.
        if let Err(e) = src.yuv_params() {
            decline("vui", e.to_string());
            return None;
        }
        Some((src, want))
    }

    /// The router. `out` decides only what the LAST assembly pass writes; every gate, decline,
    /// memo and lease below is shared, so the two doors cannot drift in what they refuse.
    fn route(
        path: &Path,
        scale_to: Option<u32>,
        lane: Lane,
        out: FinishOut,
    ) -> HwHeicAnswer {
        // Belt: the hook is only installed when the lane is live, and `decode_heic_lane` only calls
        // it on the two routed tiers. Re-asked here so neither fact is load-bearing at a distance.
        if !lane_live() || !falcon_decode::hw_heic_lane_applies(lane) {
            return HwHeicAnswer::Declined;
        }
        let Some(asm) = ASSEMBLER.get() else { return HwHeicAnswer::Declined };
        // ── v0.8.172 (G2): THE SUPERSESSION BELT, AND IT IS AT THE TOP FOR A REASON ──────────────
        //
        // The mid-grid check inside the tile loop is what makes an IN-PROGRESS decode stoppable. This
        // one is about a decode that has not started: between the worker's own pre-decode staleness
        // test and this line sit a container read, four validations and — the expensive one — a
        // `checkout` that can block up to `HW_WAIT_MS` (5000 ms) waiting for one of three sessions.
        // On the saturated engine this round is about, that window is where the browse actually
        // moves. Asking here costs one thread-local read and one atomic, and it answers before the
        // file is even opened.
        //
        // It cannot create a new re-decode class: the predicate is the calling worker's own
        // "have I moved on?", which it had already answered NO to a moment earlier, so there are
        // exactly two ways it can fire. Either the browse MOVED in between — precisely the state
        // in which the next pop would have dropped the job before decoding anyway. Or, since the
        // 2026-09-07 hover fix, THE POINTER LEFT: `hover_ask` has moved off this index while `cur`
        // is still far, so the hover preview's one exemption (`support::fast_job_stale`) has been
        // withdrawn — and a hover ask that has been withdrawn is a picture nobody is waiting for
        // any more. Abandoning its grid is the same bargain, because the id goes straight back to
        // being wanted and a returning rest re-asks for it.
        //
        // v0.8.173 (H2): …and it SAYS SO. It returned silently, which cost the setting its own
        // measurement: the instrument is the COUNT of aborts, and a browse fast enough to supersede
        // its asks before they start banked that win invisibly. One line, the same grammar as the
        // mid-grid one up to the dash, from the one place both doors pass through.
        if support::heic_speed_priority() && is_superseded() {
            log_event(&support::heic_belt_abort_line());
            return HwHeicAnswer::Superseded;
        }
        // (0)–(4): every refusal knowable before a decoder session is taken. Shared verbatim with
        // [`lane_admits`], which is the whole point of it being a function (L42).
        let Some((src, want)) = pre_checkout_gates(path, scale_to, out, asm) else {
            return HwHeicAnswer::Declined;
        };
        // (5) A SESSION. Everything above is per-file arithmetic; this is the first thing that costs
        // the machine anything, and it is where a saturated pool declines to WIC rather than queue.
        let Some(mut lease) = checkout(&src, lane, asm) else { return HwHeicAnswer::Declined };
        // (5b) v0.8.149 (F2): the PANIC lever, armed only by a dev run. Deliberately AFTER the
        // checkout — a panic before it leaks nothing, and the leak is the thing under test.
        if let Some(pat) = injected_panic_pattern() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if hw_decline_injected(name, Some(pat)) {
                panic!("FALCON_HW_HEIC_PANIC matched '{pat}' — injected panic inside the hardware \
                        HEIC decode, after the session checkout (v0.8.149 F2 falsifier)");
            }
        }
        // (6) THE DECODE. v0.8.165: `out` chooses the finish contract and nothing else — the same
        // tiles, the same composite, the same E2 kernel, the same crop/irot/resample.
        //
        // v0.8.171 (HEIC SPEED PRIORITY): …and the SETTING is read HERE, once, per decode. Off (the
        // default) hands the tile loop a predicate that is the literal `false`, so the run is
        // byte-identical to v0.8.170's. On, it hands it this thread's own signal. L43: nothing
        // latches — the setting IS an atomic and every decode re-asks it, so a flip applies to the
        // next photo with no restart, no folder reopen and nothing to un-set.
        let armed = support::heic_speed_priority();
        let mut watch = move || armed && is_superseded();
        let decoded = match out {
            FinishOut::SourceRgb => lease.dec().decode_watched(&src, scale_to, &mut watch),
            FinishOut::ManagedRgba { src: sg, dst } => lease
                .dec()
                .decode_managed_watched(&src, scale_to, sg, dst, &mut watch)
                .map(|r| match r {
                    PhotoRun::Done(p) => {
                        PhotoRun::Done(falcon_hwdec::PhotoRgb { rgb: p.rgba, w: p.w, h: p.h })
                    }
                    PhotoRun::Superseded { done, total } => PhotoRun::Superseded { done, total },
                }),
        };
        // v0.8.153 (skeptic A / O2): `falcon-hwdec` has no logger of its own — it says everything by
        // returning `Err`. Its one non-error observation is the readback note: a driver whose
        // `DepthPitch` reports the LUMA slice rather than the whole NV12 mapping, which is legal and
        // must NOT decline (v0.8.152's bound did, and would have killed the lane at boot on such a
        // box). Drained here, once per process, so the fact still reaches falcon.log.
        if let Some(note) = falcon_hwdec::take_readback_note() {
            log_event(&format!("heic hw readback: {note}"));
        }
        match decoded {
            // ── v0.8.171: THE ABORT, AND THE FOUR THINGS IT DOES NOT DO ─────────────────────────
            //
            // The session goes back to the pool with `settle(true)` — INTACT, not retired: it is
            // clean by construction (every surface released at the chunk boundary, no frame open, no
            // staging mapped) and retiring it would spend a 2–5.5 ms `CreateVideoDecoder` on the
            // next photo of this geometry AND file the retirement under `session_churn_report`'s
            // "geometry churn", which it is not.
            //
            // It does NOT `decline()`. That note is once-per-key-per-process, so the first abort
            // would burn the key and park the sentence "it decoded through WIC instead" — which is
            // false — for the whole session. It does NOT `memoise_decline()`: a post-work refusal is
            // a statement about the FILE, and this file is fine; memoising it would exile a good
            // photograph from the hardware lane for the rest of the folder. It does NOT
            // `note_hw_heic_route()`: the served/declined ledger is what promotes and demotes a
            // folder, and counting an abort as a decline would re-arm the flood protections on a
            // lane that is working perfectly — the E6 posture, poisoned by its own optimisation. And
            // it does NOT stand the lane down.
            //
            // One line per abort, and it is a `log_event` rather than a `note_decode_once` for
            // exactly the reason the others are not: an abort is a REPEATING, ordinary event on a
            // fast browse, and the number of them is the measurement.
            Ok(PhotoRun::Superseded { done, total }) => {
                lease.settle(true);
                log_event(&support::heic_abort_line(done, total));
                HwHeicAnswer::Superseded
            }
            Ok(PhotoRun::Done(p)) => {
                lease.settle(true);
                // The contract's dims clause, re-asserted on the produced photo. E3-M2 gates this on
                // every corpus file at every tier; here it is a belt, because a frame of the wrong
                // size would be filed under the tier's cache bucket and served as if it were right.
                if (p.w, p.h) != want {
                    decline(
                        "dims",
                        format!(
                            "hardware produced {}x{} where the contract promises {}x{}",
                            p.w, p.h, want.0, want.1
                        ),
                    );
                    // F8: a POST-WORK refusal — the whole hardware budget bought nothing. Remember
                    // it so the next tier and the next visit do not buy it again.
                    //
                    // v0.8.168 (F4): under BOTH DOORS, whichever one met it. `want` came from
                    // `photo::output_dims` at step (2) — above every `out`-dependent line in this
                    // function — and the finish pass derives its own size from the same geometry,
                    // so a size the hardware cannot produce is a fact about the FILE's geometry on
                    // this device and not about the colour contract. v0.8.167 filed it under `out`,
                    // which meant a managed dims mismatch left the source door open to buy the
                    // identical failure again on the next tier. (`decline`'s "it decoded through
                    // WIC instead" is therefore the true sentence here on either door: with both
                    // slots filled the hook declines too, and rung 1 is WIC.)
                    memoise_decline(path, FinishOut::SourceRgb, "dims");
                    return HwHeicAnswer::Declined;
                }
                HwHeicAnswer::Served { rgb: p.rgb, w: p.w, h: p.h }
            }
            Err(e) => {
                lease.settle(false);
                decline("decode", HwDecError::to_string(&e));
                // v0.8.168 (F4): BOTH DOORS. `decode_managed` and `decode` are one function with
                // one argument different, and that argument is read in the LAST pass only — so
                // everything that can fail here except the colour pass itself fails identically on
                // the source door, and the colour pass's own failures arrive as
                // `HwDecError::Assembly`, which is indistinguishable from a shared assembly fault
                // and stands the lane down two lines below in any case. There is no variant that
                // proves "the colour arm alone", so there is nothing to file managed-only.
                memoise_decline(path, FinishOut::SourceRgb, "decode");
                // v0.8.149 (F1) — THE DE-LATCH. Two error shapes are not about this FILE at all:
                // `DeviceLost` is the decode device gone (every session is poisoned from here), and
                // `Assembly` after step (3)'s mosaic pre-check already passed means the ONE shared
                // wgpu assembler misbehaved — a lost or removed adapter reaching us through a failed
                // buffer map. Both are properties of hardware every subsequent file would meet
                // again, so the lane stands down for the session rather than re-paying the full
                // budget per photo to rediscover it. Everything else (a driver `Api` error, a
                // bitstream this stage will not model) stays per file: the session is retired above,
                // the next file gets a fresh one.
                match &e {
                    HwDecError::DeviceLost => lane_lost("the decode device was removed"),
                    HwDecError::Assembly(why) => lane_lost(&format!("the GPU assembly failed ({why})")),
                    _ => {}
                }
                HwHeicAnswer::Declined
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// v0.8.148 (E5): the falsifier lever parses EXACTLY, the `classic_heic_from_env`
        /// precedent — `=0` means off and must not be read as "any value set".
        #[test]
        fn the_hw_disable_lever_takes_exactly_zero() {
            assert!(hw_disabled_from_env(Some("0")), "FALCON_HW_HEIC=0 disables the probe");
            assert!(!hw_disabled_from_env(Some("1")), "=1 is not a disable");
            assert!(!hw_disabled_from_env(Some("")), "empty is not a disable");
            assert!(!hw_disabled_from_env(None), "unset is not a disable");
        }

        /// v0.8.148 (E5): the MID-FOLDER decline lever selects the named file and NOTHING ELSE.
        ///
        /// The falsifier this arms is "one file declines, its neighbours do not", which is the shape
        /// of every real decline and the one a browse has to survive. A lever that matched everything
        /// (or nothing) would test the same thing the capability lever already tests.
        ///
        /// FALSIFIER (L28): make the empty pattern match and the "unarmed" rows redden — an unset
        /// variable would silently disable the whole lane.
        #[test]
        fn the_injected_decline_selects_one_file_and_leaves_its_neighbours() {
            assert!(hw_decline_injected("IMG_1827.HEIC", Some("1827")));
            assert!(!hw_decline_injected("IMG_1826.HEIC", Some("1827")), "the neighbour is untouched");
            assert!(!hw_decline_injected("IMG_1828.HEIC", Some("1827")), "…and so is the other one");
            // Unarmed, in both the ways it can be unarmed.
            assert!(!hw_decline_injected("IMG_1827.HEIC", None));
            assert!(
                !hw_decline_injected("IMG_1827.HEIC", Some("")),
                "an EMPTY pattern must not match everything — that would disable the whole lane"
            );
        }

        /// v0.8.148 (E5): THE RESERVATION. The speculative tiers may take all but one session; the
        /// interactive tier may take them all. FALSIFIER (L28): return `max` for both arms and this
        /// row reddens — the row that keeps an 18-worker prefetch flood from queueing the shot the
        /// user is actually looking at behind it.
        #[test]
        fn the_detail_tier_keeps_a_session_slot() {
            assert_eq!(session_budget(Lane::Native, 3), 3, "the interactive tier gets the pool");
            assert_eq!(session_budget(Lane::Fast, 3), 2, "the speculative tier leaves one back");
            assert_eq!(session_budget(Lane::Thumb, 3), 2, "…as the thumb tier would, if it routed");
            // A one-session pool must still be usable by both, or a small box would have a fast tier
            // that can never decode at all.
            assert_eq!(session_budget(Lane::Fast, 1), 1, "a single-session pool is not reserved away");
        }

        /// v0.8.149 (F2): THE PANIC LEVER IS INERT UNLESS ARMED, and it is armed exactly as the
        /// decline lever is.
        ///
        /// A lever that panics the decoder is the one dev instrument that MUST NOT fire by accident:
        /// unset, empty, or set to something no file matches, it has to be a no-op. It shares
        /// [`hw_decline_injected`]'s parse rather than growing a second one, so there is one set of
        /// rules for "a substring of the file NAME, typeable into a PowerShell one-liner" and one
        /// place a mistake in it could live.
        ///
        /// FALSIFIER (L28): give the panic lever its own parse that treats an empty pattern as a
        /// match and the "unarmed" rows redden — every HEIC in the corpus would panic its decode.
        #[test]
        fn the_panic_lever_is_inert_unless_it_is_armed_at_a_named_file() {
            assert!(hw_decline_injected("IMG_1827.HEIC", Some("1827")), "armed, and it matches");
            assert!(!hw_decline_injected("IMG_1826.HEIC", Some("1827")), "the neighbour decodes");
            assert!(!hw_decline_injected("IMG_1827.HEIC", None), "UNSET is inert");
            assert!(
                !hw_decline_injected("IMG_1827.HEIC", Some("")),
                "and EMPTY is inert — an empty pattern matching everything would panic every decode"
            );
            // The env door itself filters empties, so `Some("")` cannot even be produced by a run
            // that exports the variable with no value.
            assert!(
                std::env::var("FALCON_HW_HEIC_PANIC").ok().filter(|s| !s.is_empty()).is_none()
                    || injected_panic_pattern().is_some(),
                "the pattern reader and the env door agree about what 'armed' means"
            );
        }

        /// v0.8.149 (F8): THE POST-WORK DECLINE MEMO remembers within a folder and forgets across
        /// one.
        ///
        /// `dims` and `decode` are the only two declines reached AFTER a session has been taken, the
        /// tiles marshalled and the picture assembled — so a file that lands there has spent the
        /// whole hardware budget for nothing, and v0.8.148 made it spend it again per tier per
        /// visit. The memo is keyed on (folder generation, path) so it can never outlive the folder
        /// that taught it: a rescan of a folder whose files have been REPLACED must re-ask the
        /// hardware, not serve a verdict about bytes that are gone.
        ///
        /// v0.8.167: …and it is remembered PER DOOR, which is the half that was wrong. See
        /// [`DeclineMemo::blocks`].
        ///
        /// v0.8.168 (F4) — WHAT THIS ROW IS AND IS NOT. It drives `memoise_decline` directly, so it
        /// pins the MECHANISM: a refusal filed with managed scope must not close the source door.
        /// It says nothing about which scope production files, and since v0.8.168 production files
        /// BOTH DOORS for both of its refusals, because neither `dims` nor `decode` can be shown to
        /// be a fault of the colour arm alone (the reasoning is in the module block above the
        /// struct). The row stays because the asymmetry is the thing that will be needed the day
        /// `HwDecError` grows a colour-pass variant, and an untested asymmetry is one nobody can
        /// then trust.
        ///
        /// FALSIFIER (L28): drop the generation from the key and the swap row reddens; clear the map
        /// on every lookup and the "remembered within the folder" row reddens; make `blocks` answer
        /// `self.source.or(self.managed)` for BOTH doors (the v0.8.166 behaviour, one field short)
        /// and the row that matters here — a managed-scoped refusal must not close the source door —
        /// reddens.
        #[test]
        fn a_post_work_decline_is_remembered_for_the_folder_and_forgotten_on_a_swap() {
            // v0.8.187 (FRESH-1): REAL files, because the key carries their mtime now. The paths
            // were `C:/nowhere/...` while the key was the name alone; a memo keyed on bytes has
            // to be driven with bytes. PID-scoped so parallel `cargo test` runs cannot collide.
            let dir = std::env::temp_dir().join(format!("falcon_decline_memo_{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let mk = |n: &str| {
                let p = dir.join(n);
                std::fs::write(&p, b"heic").expect("write the memo fixture");
                p
            };
            let (ap, bp) = (mk("F001_IMG_1826.HEIC"), mk("F002_IMG_1827.HEIC"));
            let (a, b) = (ap.as_path(), bp.as_path());
            let hook = FinishOut::SourceRgb;
            let colour = FinishOut::ManagedRgba {
                src: falcon_color::Gamut::DisplayP3,
                dst: falcon_color::Gamut::Srgb,
            };
            note_folder_swap(); // start from a known generation, whatever ran before
            assert_eq!(memoised_decline(a, hook), None, "a fresh folder knows nothing");
            memoise_decline(a, hook, "dims");
            assert_eq!(
                memoised_decline(a, hook),
                Some("dims"),
                "…and remembers it for every later tier and visit IN THIS FOLDER"
            );
            assert_eq!(
                memoised_decline(a, colour),
                Some("dims"),
                "a SOURCE-door refusal blocks the managed door too: same tiles, same composite"
            );
            assert_eq!(memoised_decline(b, hook), None, "one file's verdict is not the folder's");

            // THE ROW THE MECHANISM EXISTS FOR: a refusal whose FAULT is the colour arm alone does
            // not take the hardware lane away from the file — it must still decode on the GPU's
            // video engine, with `falcon_color::transform_rgba` doing its colour. (v0.8.168: the
            // scope, not the door — no shipping writer files this today; see the doc above.)
            memoise_decline(b, colour, "decode");
            assert_eq!(memoised_decline(b, colour), Some("decode"), "the managed door remembers");
            assert_eq!(
                memoised_decline(b, hook),
                None,
                "…and the SOURCE door is untouched — a colour-arm failure must not demote the \
                 file to WIC for the rest of the folder"
            );

            note_folder_swap();
            assert_eq!(
                memoised_decline(a, hook),
                None,
                "a swap forgets: the same path in a new folder may be different bytes"
            );
            assert_eq!(memoised_decline(b, colour), None, "…on both doors");

            // ── v0.8.173 (H4): THE THREE PARSE-TIME REFUSALS ARE REMEMBERED NOW ─────────────────
            //
            // They were left out while the only caller was a decode, on the reasoning that a file
            // that cannot be parsed is re-parsed. v0.8.172 (G6) gave the parse a SECOND caller —
            // `lane_admits`, the one-decode gate's pre-check — so an unparseable file paid its
            // ~11 MB read and full container parse twice per ask, then again per tier and per
            // visit. Filed under SourceRgb scope, which is the half both doors share.
            let cp = mk("F003_IMG_1828.HEIC");
            let c = cp.as_path();
            for reason in ["container", "bitdepth", "geometry"] {
                note_folder_swap();
                assert_eq!(memoised_decline(c, hook), None, "{reason}: a fresh folder knows nothing");
                memoise_decline(c, hook, reason);
                assert_eq!(
                    memoised_decline(c, hook),
                    Some(reason),
                    "{reason} is a fact about the FILE — the second ask must not re-read 11 MB to \
                     rediscover it"
                );
                assert_eq!(
                    memoised_decline(c, colour),
                    Some(reason),
                    "…and it blocks BOTH doors: the parse is the half they share"
                );
                note_folder_swap();
                assert_eq!(
                    memoised_decline(c, hook),
                    None,
                    "…and it is still bounded by the folder, so a rescan re-asks the hardware"
                );
            }

            // ── v0.8.173 (H11): AN ABORT WRITES NOTHING HERE ────────────────────────────────────
            //
            // `an_abort_moves_no_ledger_no_memo_and_no_latch` (support.rs) names three obligations
            // and could reach only one; this is the MEMO half, asserted where the state lives and
            // INSIDE this row rather than beside it, because `DECLINE_MEMO` and `FOLDER_GEN` are
            // process-wide statics and `cargo test` runs rows in parallel — a second row calling
            // `note_folder_swap` would wipe this one's generation mid-assertion. (The L43 lesson
            // from B-Y13, applied to a different shared cell.)
            //
            // WHAT IT PINS AND WHAT IT CANNOT: it drives the values an abort actually produces —
            // both abort lines and the `Superseded` answer both doors return — past the memo, and
            // asserts the memo is still empty for that file. What it cannot do is run `route()`'s
            // abort arm, which needs a video device and a real 48 MP grid; that arm's guarantee is
            // structural and enumerated at the arm (no `decline`, no `memoise_decline`, no
            // `note_hw_heic_route`, no `lane_lost`).
            //
            // FALSIFIER (L28): add `memoise_decline(path, FinishOut::SourceRgb, "superseded")` to
            // the router's abort arm — the plausible mistake, since every other non-serving outcome
            // there files one — and this block reddens on both doors.
            let dp = mk("F004_IMG_1829.HEIC");
            let d = dp.as_path();
            note_folder_swap();
            assert_eq!(memoised_decline(d, hook), None, "a fresh folder knows nothing");
            let mid = support::heic_abort_line(16, 190);
            let belt = support::heic_belt_abort_line();
            assert!(mid.contains("nothing is memoised"), "the mid-grid line claims it: {mid}");
            assert!(belt.contains("nothing is memoised"), "…and so does the belt's: {belt}");
            assert!(matches!(
                support::managed_answer(HwHeicAnswer::Superseded),
                support::ManagedAnswer::Superseded
            ));
            assert_eq!(memoised_decline(d, hook), None, "an abort is not a refusal of this FILE");
            assert_eq!(memoised_decline(d, colour), None, "…on either door");
            // …and the consequence that would be lost, stated as the assertion that matters: a
            // memoised abort makes the very next ask decline before the container is even read.
            memoise_decline(d, hook, "decode");
            assert_eq!(
                memoised_decline(d, hook),
                Some("decode"),
                "…which is what a memo entry DOES to the next ask, and why an abort must not write one"
            );

            // ── v0.8.187 (FRESH-1, ledger L42): THE BYTES TERM, DIFFERENTIALLY ──────────────────
            //
            // The entry filed one assert above is against THESE bytes. Rewrite the file in place —
            // the phone-still-syncing / export-overwriting case its neighbour `PLAN_CACHE` names —
            // and the memo must fall silent, because a refusal of the old bytes is not evidence
            // about the new ones. Nothing else moves: same path, same folder generation.
            //
            // RED-FIRST: with the pre-v0.8.187 path-only key this asserts `None` and gets
            // `Some("decode")`, i.e. a HEIC the device can now decode stays demoted to WIC for the
            // rest of the folder. The mtime must really differ, so the write is preceded by a short
            // sleep — NTFS timestamps are 100 ns but `SystemTime::now()` on Windows advances on the
            // ~15.6 ms system-clock tick, and two writes inside one tick can share an mtime.
            std::thread::sleep(std::time::Duration::from_millis(40));
            std::fs::write(d, b"heic-rewritten").expect("rewrite the memo fixture");
            assert_eq!(
                memoised_decline(d, hook),
                None,
                "a file rewritten under the same name is re-asked: the key is (path, mtime), never \
                 the path alone (L42) — the rule this memo's neighbour PLAN_CACHE already states"
            );
            // …and the re-file replaces rather than merges, so the SECOND door starts clean too:
            // the old bytes' managed-scope verdict must not survive into the new bytes' entry.
            memoise_decline(d, colour, "decode");
            assert_eq!(memoised_decline(d, colour), Some("decode"), "the new bytes get their own memo");
            assert_eq!(memoised_decline(d, hook), None, "…and only their own");

            note_folder_swap();
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// v0.8.165 (WAVE 1): THE GPU-COLOUR LEVER takes exactly `0`, like every other lever in
        /// this module — and the DEFAULT is ON, which is the half a parse test has to pin.
        ///
        /// FALSIFIER (L28): make the parse `v.is_some()` (the plausible mistake — "the variable is
        /// set, so the feature is off") and the `=1` row reddens: a run that exported
        /// `FALCON_HEIC_GPU_COLOR=1` to be explicit would have silently got the CPU arm, and the
        /// measurement pass that A/Bs the two arms would have measured one of them twice.
        #[test]
        fn the_gpu_colour_lever_takes_exactly_zero_and_defaults_on() {
            assert!(gpu_color_disabled_from_env(Some("0")), "=0 reverts to the CPU colour path");
            assert!(!gpu_color_disabled_from_env(Some("1")), "=1 is not a disable");
            assert!(!gpu_color_disabled_from_env(Some("")), "empty is not a disable");
            assert!(!gpu_color_disabled_from_env(None), "UNSET is the product: the GPU arm is on");
        }

        /// v0.8.165 (WAVE 1, iGPU deliverable 2): THE RENDERER PRE-CHECK asks the device, and
        /// refuses what it has not measured.
        ///
        /// The numbers in the rows are the two real ones: 32768 is what this box's renderer
        /// reports and 16384 is what an Intel iGPU commonly reports, so an 8064x6048 frame fits
        /// both while a 20000 px one fits neither — and a CONSTANT in place of the device's answer
        /// would be wrong on one of them the moment the corpus grew.
        ///
        /// FALSIFIER (L28): return `Some(true)` for `limit == 0` and the LAST assert reddens. That
        /// is the live case, not a hypothetical: the hardware probe lands a few hundred ms into
        /// boot and `RenderingSetup` can land AFTER it, so the very first folder's first frames
        /// really do ask this question before any limit has been captured.
        #[test]
        fn the_renderer_precheck_refuses_what_it_has_not_measured() {
            assert_eq!(renderer_holds(8064, 6048, 32768), Some(true), "a 48 MP frame on this box");
            assert_eq!(renderer_holds(8064, 6048, 16384), Some(true), "…and on a 16384 iGPU");
            assert_eq!(renderer_holds(6048, 8064, 16384), Some(true), "…rotated, too");
            assert_eq!(renderer_holds(20000, 100, 16384), Some(false), "wide past a 16384 cap");
            assert_eq!(renderer_holds(100, 20000, 16384), Some(false), "…and tall past it");
            assert_eq!(
                renderer_holds(8064, 6048, 0),
                None,
                "an UNMEASURED limit is not a permission — a GPU-coloured frame goes straight into \
                 a renderer texture with nothing in between that could resize it"
            );
        }

        /// v0.8.148 (E5): THE TOPOLOGY VERDICT is a function of two measured numbers, and it says
        /// something DIFFERENT in the two regimes rather than one sentence that fits both.
        ///
        /// The measured case on this box is the first: wgpu's default storage binding is 134217728 B
        /// and a 48 MP mosaic is 198180864 B, so the renderer's device could not hold the owner's own
        /// corpus and the dedicated device is decided by capability, not preference.
        #[test]
        fn the_topology_verdict_names_the_number_that_decided_it() {
            let dedicated = topology_verdict(2 << 30, Some(134_217_728));
            assert!(dedicated.contains("DECIDED BY MEASUREMENT"), "{dedicated}");
            assert!(dedicated.contains("134217728"), "it quotes the renderer's real limit");
            assert!(dedicated.contains(&MOSAIC_48MP_BYTES.to_string()), "…against a real photo");
            let roomy = topology_verdict(2 << 30, Some(2 << 30));
            assert!(roomy.contains("isolation rather than capability"), "{roomy}");
            // THE MEASURED CASE on this box, and the reason `Option` replaced a 0 sentinel: Slint's
            // device really does report zero, and "measured, and the answer is none" is a stronger
            // statement than "not measured" — not a synonym for it.
            let zero = topology_verdict(2 << 30, Some(0));
            assert!(zero.contains("DECIDED BY MEASUREMENT"), "{zero}");
            assert!(zero.contains("NO storage buffer at all"), "{zero}");
            assert!(zero.contains("admits 0 B per storage binding"), "the measured figure: {zero}");
            let early = topology_verdict(2 << 30, None);
            assert!(early.contains("had not been captured"), "{early}");
            assert!(!early.contains("DECIDED BY MEASUREMENT"), "an unmeasured line claims nothing");
        }

        /// v0.8.157 (§3, ruled 5.2b): A TILE PLAN IS NEVER SERVED FOR BYTES IT DID NOT COME FROM.
        ///
        /// The cache exists to stop the ~11 MB container re-read + re-parse a photograph pays once
        /// per attempt (fast on the way past, native when the user stops, fast again on the way
        /// back). Everything that makes that SAFE is in this predicate: the folder generation bounds
        /// it (L43 — a plan cannot outlive the folder that taught it, and the READ asks as well as
        /// the clear) and the mtime keys it (L42 — the key is (path, mtime), so a file rewritten
        /// under the same name while the folder is open is re-parsed, never answered from bytes that
        /// are gone).
        ///
        /// FALSIFIER (L28): delete the `entry_mtime == file_mtime` term from `plan_cache_serves` and
        /// the REWRITTEN row below fails — that is the L42 defect class, shipped. Delete the
        /// `entry_gen == gen_now` term and the SWAPPED row fails — a plan surviving into a folder it
        /// does not describe, which is the decline memo's own bug (F8) in a second place.
        #[test]
        fn a_tile_plan_is_never_served_for_bytes_it_did_not_come_from() {
            let t0 = std::time::SystemTime::UNIX_EPOCH;
            let t1 = t0 + Duration::from_secs(1);
            assert!(plan_cache_serves(7, 7, t0, t0), "same folder, same bytes → the parse is reused");
            assert!(
                !plan_cache_serves(7, 7, t0, t1),
                "REWRITTEN under the same name: the mtime moved, so the plan describes bytes that \
                 are no longer there and must be re-parsed"
            );
            assert!(
                !plan_cache_serves(7, 8, t0, t0),
                "SWAPPED folder: the same path may be a different photograph now"
            );
            assert!(!plan_cache_serves(7, 8, t0, t1), "both stale is still stale");
        }

        /// v0.8.160 (P3/U6): A PARSE THAT BELONGS TO A FOLDER THAT IS GONE IS NOT FILED AT ALL.
        ///
        /// `cached_tile_source` reads the generation, then spends ~11 MB of read and a full parse,
        /// then files. A folder swap inside that window used to file under the ORPHAN'S generation:
        /// the first such straggler left the map stamped dead (so the NEW folder's reads all missed
        /// and paid the re-read + re-parse §3 exists to remove), and every later straggler WIPED
        /// whatever the new folder had banked and re-stamped it dead again — once per straggler,
        /// during exactly the folder-open storm this cache flattens.
        ///
        /// The two generations are the whole test: the parse's and the live one.
        ///
        /// FALSIFIER (L28): delete the `parse_gen != live_gen` guard from `plan_cache_slot` (or key
        /// its reset on `parse_gen`, which is what shipped) and the DECLINE block below fails — the
        /// live folder's cache comes back emptied and stamped with a dead generation.
        #[test]
        fn a_plan_parsed_in_a_folder_that_is_gone_is_never_filed() {
            let live_path = PathBuf::from("C:/nowhere/LIVE.HEIC");
            // The LIVE folder's cache: generation 9, one entry's worth of accounting already held.
            // (The map itself stays empty — the accounting and the insertion order are what a wipe
            // destroys, and they need no `TileSource` to be observed.)
            let mut slot: Option<PlanSlot> = Some((
                9,
                HashMap::new(),
                std::collections::VecDeque::from(vec![live_path.clone()]),
                11_000_000,
            ));

            // (1) A STRAGGLER from generation 8 lands. It is declined, and NOTHING about the live
            //     folder's cache moves.
            assert!(
                plan_cache_slot(&mut slot, 8, 9).is_none(),
                "a parse from the previous folder must not be filed"
            );
            let (gen, _, order, bytes) = slot.as_ref().expect("the live cache still exists");
            assert_eq!(*gen, 9, "…and the live cache is still stamped with the LIVE generation");
            assert_eq!(order.len(), 1, "…its insertion order is intact");
            assert_eq!(*bytes, 11_000_000, "…and so is its byte accounting");

            // (2) A parse from the LIVE generation is filed into that same slot — the cache the
            //     straggler would have destroyed is the one this folder goes on using.
            {
                let entry = plan_cache_slot(&mut slot, 9, 9).expect("a live parse files");
                assert_eq!(entry.0, 9);
                assert_eq!(entry.3, 11_000_000, "an admitted filing joins the live cache, it does not reset it");
            }

            // (3) THE BELT: an empty slot (a fresh folder, or one a swap has just cleared) is created
            //     at the live generation, and a slot somehow left stamped stale is reset to it.
            let mut fresh: Option<PlanSlot> = None;
            assert_eq!(plan_cache_slot(&mut fresh, 9, 9).expect("created").0, 9);
            let mut stale: Option<PlanSlot> =
                Some((3, HashMap::new(), std::collections::VecDeque::new(), 77));
            let entry = plan_cache_slot(&mut stale, 9, 9).expect("reset");
            assert_eq!((entry.0, entry.3), (9, 0), "a stale map is emptied and re-stamped LIVE");
        }

        /// v0.8.157 (§3): THE BYTE BUDGET, and why the folder generation alone is not the bound the
        /// ruling assumed.
        ///
        /// A tile PLAN is small; a `TileSource` is not — it carries `tiles: Vec<Vec<u8>>`, which on a
        /// 48 MP iPhone photo is nearly the whole 11 MB file. Bounded only by the folder generation,
        /// a 100-file phone folder would park ~1 GB of compressed tiles beside an 8 GB L2: a memory
        /// regression traded for a parse. `plan_bytes` is what the budget counts, and it counts the
        /// payloads because they are the entire cost.
        ///
        /// FALSIFIER (L28): make `plan_bytes` return a constant (or count only the geometry) and this
        /// reddens — the budget would then admit an unbounded number of 11 MB entries.
        #[test]
        fn the_plan_cache_charges_for_the_tile_payloads_because_they_are_the_cost() {
            // Two 4 MB tiles: the budget must see 8 MB, not "one plan".
            let tiles = vec![vec![0u8; 4 * 1024 * 1024], vec![0u8; 4 * 1024 * 1024]];
            let bytes = plan_bytes(&tiles);
            assert_eq!(bytes, 8 * 1024 * 1024, "the payloads ARE the cost");
            assert_eq!(plan_bytes(&[]), 0, "…and a plan with no tiles costs nothing");
            assert!(
                PLAN_CACHE_BYTES / bytes >= 8 && PLAN_CACHE_BYTES / bytes <= 64,
                "the budget holds a browse neighbourhood's worth of plans, not a folder's: \
                 {PLAN_CACHE_BYTES} B / {bytes} B"
            );
        }
    }
}
