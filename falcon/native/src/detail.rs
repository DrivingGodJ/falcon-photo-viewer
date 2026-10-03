//! v0.8.30 (architecture round 2, PLAN §64): the DETAIL-TIER subsystem. ONE struct owning the
//! scattered full-res (detail) decode-tier state so its clearing happens BY CONSTRUCTION — the same
//! pattern [`crate::FastTier`] established in round 1 — instead of the hand-maintained checklists in
//! `apply_scan` + `drop_developed_caches` + three develop-config handlers that history keeps proving
//! fragile (the §61 `frost_map` miss; the v0.8.13→15 delete-identity episode). `main()` used to clone
//! FOUR separate detail handles into the tick closure and thread them through the `step_*` signatures;
//! they now live here and one `Rc<DetailTier>` clones in.
//!
//! BEHAVIOR-IDENTICAL: this round MOVES state and changes NO logic. The four stores keep the exact
//! interior-mutability discipline they had as loose handles (all four are UI-thread-only `Rc<RefCell>`
//! / `Rc<Cell>` — unlike FastTier, NO field is worker-shared: the detail worker communicates only via
//! the `det_rx` channel + the `det_busy`/`det_epoch` atomics, which stay OUT). The clearing semantics
//! below match the pre-extraction lines byte-for-byte (proven by `cargo test` + a boot-metric parity
//! spike on the testkit + the 66-shot folder).
//!
//! MEMBERSHIP (the conservative boundary — only unambiguously detail-tier-OWNED, cleared-on-swap state):
//!   • `cache`     — the full-res detail VRAM textures, developed/colour-managed INTO the output gamut
//!                   (value = the decoded `slint::Image` + its w,h). The [`tick::DetailCache`] map.
//!                   UI-thread-only (was `detail_cache`).
//!   • `order`     — the LRU companion to `cache`: insertion/touch order for the byte-budget eviction
//!                   in `step_upload_drain`. Cleared in LOCK-STEP with `cache` or it dangles stale keys
//!                   (was `detail_order`). UI-thread-only.
//!   • `failed`    — the per-shot full-res-develop failure latch: a shot whose detail decode Err'd
//!                   (unsupported/corrupt, or an HEIC with no OS HEVC extension) is skipped so the
//!                   worker isn't re-fed the same doomed job every tick (the retry-storm guard, detail
//!                   edition). Bare index (NOT (gen,idx)) — it is UI-thread-only: the TICK inserts on a
//!                   w=0 marker, the prefetch reads. Cleared on swap + develop-config change + user
//!                   retry (was `det_failed`).
//!   • `uploading` — the SINGLE in-flight detail-upload slot (`Some(id)` while a frame is on the U1
//!                   upload thread; single-flight, like the worker). Prefetch/upload treat it as in
//!                   progress; the drain clears it (was `det_uploading`, `Rc<Cell<Option<usize>>>`).
//!
//! DELIBERATELY OUT (shared with other consumers, not folder-swap-cleared, or channel/atomic plumbing —
//! see the round-2 report):
//!   `det_epoch` (the develop-config epoch `AtomicU64` — MONOTONIC, never reset per-folder; bumped by
//!   ~6 non-detail-worker handlers: RAW toggle, output-gamut, ROI hi-res toggle, res-limit, sim-VRAM;
//!   read by the detail WORKER off-thread to reject stale-config landers — exactly the cross-consumer
//!   config atomic FastTier parked as `fast_super`), `det_busy` (the one-decode-in-flight `AtomicBool`
//!   tick↔worker handshake — set-true-before-send in the tick, set-false-on-completion in the worker;
//!   NOT swap-cleared), `det_sent` (the tick-CLOSURE-local `RefCell<HashSet>` dedup latch — never an
//!   `Rc`, unreachable from `apply_scan`, self-drains as each `Detail` result is dequeued and is NOT
//!   swap-cleared; folding it in would ADD a swap-clear it doesn't have today = a behaviour change),
//!   the `Detail` result channel (`det_rx` — parity with FastTier excluding the `Decoded` channel),
//!   and `detail_budget` / `detail_cap` / `detail_dim_atomic` (the VRAM-budget tier state co-managed
//!   with `fast_budget` inside the `VramTier` value-struct + the OOM-recovery / sim-VRAM control —
//!   parity with `fast_budget`). Folding any of these in is a mechanical follow-up.
use crate::*;

/// The detail-decode tier's owned state (see the module doc for the membership boundary). Wrapped in an
/// `Rc<DetailTier>` on the UI thread; unlike [`crate::FastTier`] NO field is worker-shared, so nothing
/// here is `Arc` — the detail worker never touches these stores (it replies on `det_rx`; the TICK is
/// the sole writer of `cache`/`order`/`failed`/`uploading`).
pub(crate) struct DetailTier {
    pub(crate) cache: RefCell<tick::DetailCache>,
    pub(crate) order: RefCell<VecDeque<usize>>,
    pub(crate) failed: RefCell<HashSet<usize>>,
    pub(crate) uploading: Cell<Option<usize>>,
}

impl DetailTier {
    pub(crate) fn new() -> DetailTier {
        DetailTier {
            cache: RefCell::new(HashMap::new()),
            order: RefCell::new(VecDeque::new()),
            failed: RefCell::new(HashSet::new()),
            uploading: Cell::new(None),
        }
    }

    /// Drop the DEVELOPED detail tier — the output-gamut-dependent VRAM textures (`cache`), their LRU
    /// companion (`order`), and the per-shot develop-failure latch (`failed`). This is the ONE
    /// definition of "the detail tier's developed set", and every site that must re-develop every shot
    /// under a new config funnels through it (map old-line → method in the round-2 report):
    ///   • `drop_developed_caches` — the LV5 gamut/ICC chokepoint (§6.0).
    ///   • [`on_folder_swap`](Self::on_folder_swap) — a swap re-keys every index, so the developed set
    ///     is meaningless (plus the swap-EXTRA below).
    ///   • the RAW-develop toggle, the ROI-hi-res toggle, the dev sim-VRAM control, and (v0.8.187)
    ///     the RES-LIMIT handler — each re-decodes every shot in the new mode/cap.
    ///
    /// DELIBERATELY does NOT touch `uploading`: the develop-config handlers bump `det_epoch` instead, so
    /// an in-flight old-config upload is rejected by the tick's epoch guard and the drain self-clears the
    /// slot. Only a folder SWAP clears `uploading` (see below).
    ///
    /// ── v0.8.187 (W4): THE ODD ONE OUT IS RETIRED, BECAUSE ITS PREMISE WAS FALSE ─────────────────
    /// This paragraph used to record the res-limit handler as the one call site that clears
    /// {cache, order} and deliberately KEEPS `failed`, "a size-cap change can't fix a corrupt-file
    /// failure". A size-cap change is exactly what fixes one class of full-res failure:
    /// `falcon-decode::decode_jpeg`'s scaled arm asks `jpeg_decoder` for the smallest DCT stop whose
    /// long side is >= `detail_dim_atomic`, and `guard_source_dims` REFUSES an over-large stop — so
    /// on a big source with Adaptive Hi-Res off and the manual limit high, the decode Errs and
    /// latches, and lowering the limit (which is what makes that same file decodable) used to leave
    /// the latch standing. The handler routes through this method now, and both `step_vram_recovery`
    /// rungs — the other two `ddim` writers that could not reach the store — clear it too, INSIDE
    /// the gates that actually move the number (v0.8.187 X6: neither rung writes `ddim`
    /// unconditionally, and on a lap that moves nothing the latch must STAY, or the OOM lap re-arms
    /// the oversized decode it is relieving).
    ///
    /// THE CENSUS: `detail_dim_atomic` has SIX store sites — boot, the hi-res toggle, the res-limit
    /// handler, Simulate VRAM, and the ladder's two rungs. Each drops the full-res failure latch
    /// WHEN IT MOVES THE NUMBER; the converse is deliberately not claimed.
    pub(crate) fn clear_developed(&self) {
        self.cache.borrow_mut().clear();
        self.order.borrow_mut().clear();
        self.failed.borrow_mut().clear();
    }

    /// Folder-swap clearing, BY CONSTRUCTION. Absorbs the detail-tier entries `apply_scan` used to run
    /// as scattered hand-written lines: the developed set via [`clear_developed`](Self::clear_developed)
    /// (detail VRAM cache + its LRU + the failure latch) PLUS the swap-EXTRA — the in-flight
    /// detail-upload slot (`uploading`), which the gamut/config paths deliberately leave alone. Called
    /// from the ONE `apply_scan` swap chokepoint.
    ///
    /// SEMANTICS preserved EXACTLY (the round-2 contract — a skeptic should check these):
    ///   • `cache` / `order` / `failed` — on the SWAP path these are cleared TWICE: once by
    ///     `drop_developed_caches` (which the swap still routes through, so the GAMUT path also drops
    ///     them) and once here via `clear_developed`. That double-clear is a DELIBERATE, harmless no-op:
    ///     the second `clear()` runs on already-empty collections, and NO reader/writer runs between the
    ///     two on the UI thread (the intervening `apply_scan` lines touch only OTHER per-folder stores;
    ///     the detail worker never writes these — the TICK does, and no tick step runs mid-`apply_scan`).
    ///     They STAY in `drop_developed_caches` because a GAMUT/ICC change — a different call site that
    ///     never reaches this method — must also drop these colour-managed textures (LV5 §6.0). So the
    ///     two chokepoints intentionally overlap, and both remain correct. This mirrors the FastTier
    ///     round-1 `cache` double-clear exactly.
    ///   • `uploading` — cleared to `None` here (was the standalone `det_uploading.set(None)` line in
    ///     `apply_scan`). The old marker is folder-stale; the in-flight result still lands and would
    ///     clear it gen-checked, but clearing on swap lets the new folder's detail prefetch start at
    ///     once. `clear_developed` does NOT touch it, so it is cleared exactly ONCE on the swap path.
    ///   • ORDER among the four is irrelevant: they are independent stores and no reader runs between
    ///     them on the UI thread at the swap chokepoint.
    pub(crate) fn on_folder_swap(&self) {
        self.clear_developed();
        self.uploading.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 placeholder frame — the same no-backend `slint::Image` construction the
    /// `rotate_invalidate_tests` (and fast.rs) use, so a detail cache entry can be built under
    /// `cargo test` (no event loop).
    fn img() -> slint::Image {
        slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(1, 1))
    }

    /// Populate EVERY owned store to a known non-empty state (used by both clearing tests).
    fn populate(d: &DetailTier) {
        d.cache.borrow_mut().insert(7, (img(), 8192, 5464));
        d.order.borrow_mut().push_back(7);
        d.failed.borrow_mut().insert(9);
        d.uploading.set(Some(7));
    }

    /// The direct swap-clear test (the round-1 review's "cheap first target", detail edition): populate
    /// every owned store, call `on_folder_swap`, assert each is empty — INCLUDING the `uploading` slot
    /// (the swap-EXTRA that `clear_developed` alone leaves alone). Before this round the only proof that
    /// a folder swap clears the detail tier was a live boot; now it is a unit test that can't rot.
    #[test]
    fn detail_tier_swap_clears_everything() {
        let d = DetailTier::new();
        populate(&d);

        d.on_folder_swap();

        assert!(d.cache.borrow().is_empty(), "detail VRAM cache cleared on swap");
        assert!(d.order.borrow().is_empty(), "detail LRU order cleared on swap");
        assert!(d.failed.borrow().is_empty(), "detail failure latch cleared on swap");
        assert_eq!(d.uploading.get(), None, "in-flight detail upload slot cleared on swap");
    }

    /// The TWO-PATH distinction, pinned: `clear_developed` (the gamut/ICC + develop-config path) drops
    /// EXACTLY the developed set {cache, order, failed} and must LEAVE `uploading` untouched — those
    /// paths bump `det_epoch` instead, and only a folder SWAP clears the slot. If a future edit folds
    /// `uploading` into `clear_developed`, a gamut change mid-upload would wrongly free the slot and this
    /// test fails.
    ///
    /// ── v0.8.187 (W4): THE SCENARIO THE `failed` LINE BELOW IS NOW LOAD-BEARING FOR ─────────────
    ///
    /// Until this round the res-limit handler and both `step_vram_recovery` rungs cleared
    /// {cache, order} by hand and deliberately KEPT `failed`, on the premise that "a size-cap change
    /// can't fix a corrupt-file failure". Here is the reachable case that falsifies it, end to end:
    ///
    ///   1. Adaptive Hi-Res is OFF, so `detail_dim_atomic` is the user's manual Resolution limit —
    ///      16384 by default. The photograph is a large JPEG (a stitched panorama, a 100 MP file).
    ///   2. The detail worker asks `falcon_decode::decode_jpeg(bytes, Some(16384))`, whose scaled
    ///      arm calls `jpeg_decoder::Decoder::scale(16384, 16384)`. That API NEVER UNDERSHOOTS: it
    ///      returns the smallest DCT stop whose long side is >= the request, so a huge source lands
    ///      on the 1/1 or 1/2 stop.
    ///   3. `guard_source_dims(sw, sh, "JPEG (scaled)")` — the decompression-bomb guard on the
    ///      ACTUAL stop size — refuses it. The decode returns `Err`, and the tick latches the shot
    ///      in `failed` so the worker is not re-fed the same doomed job every tick.
    ///   4. The user drags the Resolution limit DOWN. The smaller request selects a smaller stop,
    ///      which passes the guard — i.e. the very control the user reached for is what makes this
    ///      file decodable — and the handler cleared the cache, the LRU and the epoch, and left the
    ///      latch that was the only thing still blocking it. The photograph stayed soft until a
    ///      folder swap. The same holds for both VRAM-ladder rungs, which also move that number.
    ///
    /// So the four develop-config handlers route through this method (the two VRAM-ladder rungs
    /// clear `failed` directly through their threaded param — six store sites in all), and the `failed`
    /// assert below is what keeps them uniform: fold `failed` OUT of `clear_developed` and every one
    /// of them silently re-opens the wedge.
    #[test]
    fn clear_developed_clears_exactly_the_developed_set() {
        let d = DetailTier::new();
        populate(&d);

        d.clear_developed();

        assert!(d.cache.borrow().is_empty(), "clear_developed drops the detail cache");
        assert!(d.order.borrow().is_empty(), "clear_developed drops the LRU order");
        assert!(d.failed.borrow().is_empty(), "clear_developed drops the failure latch");
        assert_eq!(
            d.uploading.get(),
            Some(7),
            "clear_developed MUST preserve the in-flight upload slot (only a folder swap clears it)"
        );
    }

    /// Idempotence: the `cache`/`order`/`failed` double-clear on the swap path (drop_developed_caches
    /// THEN on_folder_swap) is a no-op on already-empty collections — this pins that so a future reader
    /// can't mistake the overlap for a bug. Also proves a second swap on a fresh tier stays clean.
    #[test]
    fn on_folder_swap_is_idempotent() {
        let d = DetailTier::new();
        d.on_folder_swap();
        d.on_folder_swap();
        assert!(d.cache.borrow().is_empty());
        assert!(d.order.borrow().is_empty());
        assert!(d.failed.borrow().is_empty());
        assert_eq!(d.uploading.get(), None);
    }
}
