//! v0.8.166 (WAVE 2), narrowed by the 08-06 audit in v0.8.167 — **the band plan**: how one big
//! transfer becomes several small ones. It has exactly ONE consumer: the HEIC assembly's readback
//! (`heic::read_back_planned`).
//!
//! # What this module is, and what it deliberately no longer is
//!
//! WAVE 2 shipped this planner to TWO places: the HEIC readback on the assembly device, and the
//! renderer-side texture writes (`support::create_texture_rgba` / `create_texture_cm` /
//! `YuvConvert::convert`). The renderer-side half is REVERTED, wholesale, and the reasoning is
//! recorded here so it is not re-proposed from the same plausible premise:
//!
//! * THE DATA DEPENDENCY. The frame that displays a new photo SAMPLES the published texture. Every
//!   band of that texture must therefore complete before that frame can draw, whoever submitted
//!   them — so cutting the copy into 12 submissions moves the wait around, it does not remove it.
//!   The measurement agreed: the acceptance metric did not move.
//! * THE CORRELATION WAS PARTLY DEFINITIONAL. `r(publish, gap p95)` = 0.74–0.95 read like "the
//!   publish IS the gap", but both spans contain the UI thread's own scheduling latency, so the two
//!   were guaranteed to move together whatever the copy did.
//! * AND THE SLICING COST REAL THINGS: wgpu tracks partial-copy initialisation, so a banded write
//!   into a fresh texture emitted a full-surface zero-clear (378 zero-buffer regions on a 48 MP
//!   frame); 22 device-global lock acquisitions replaced one; a blocking `poll` landed in the
//!   fast-scrub hot path; and under a 4 GiB budget the WEAKEST hardware got the MOST submits.
//!
//! The readback keeps it because the readback is a different shape: it is a device→host DMA
//! followed by a CPU unpack, on the ASSEMBLY device, which Slint's queue never touches — so banding
//! there overlaps the two halves against each other rather than fighting a data dependency. See
//! [`crate::heic::read_back_planned`] for the measured −20 %.
//!
//! # The iGPU clause (owner's standing directive)
//!
//! `wgpu_core` allocates a fresh HOST-VISIBLE, mapped buffer for the readback destination. Whole: a
//! ~195 MB mapped allocation per 48 MP photo. Banded: two buffers of one band each — 32 MiB, peak,
//! whatever the picture size — and on an integrated GPU every one of those bytes is SYSTEM RAM
//! shared with the app's own working set (`vram_budget_bytes` returns `SharedSystemMemory/4`
//! there). A shared-memory box therefore benefits MOST, twice over: the transient allocation
//! shrinks by ~6×, and the copy it feeds competes with the display for the same memory controller
//! in small pieces instead of one burst.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes one band aims to carry. 16 MiB is chosen against the frame budget, not against the picture
/// size: at the ~10 GB/s this class of link sustains, one band is ~1.6 ms of DMA against ~2 ms of
/// CPU unpack — close enough that the two halves overlap nearly perfectly, which is the whole point
/// of the pipeline. Sizing by TIME rather than by a fraction of the picture is deliberate: a
/// fraction would keep both halves scaling with the photo.
pub const BAND_TARGET_BYTES: u64 = 16 << 20;

/// A transfer at or under this stays WHOLE — one copy, one map, one unpack, byte-identical to the
/// v0.8.165 path: do not tax the cheap case with scheduling overhead. At 4 bytes per pixel 24 MiB
/// is ~6 MP, so every fast-tier HEIC finish stays whole and the full-res ones (48 MP ≈ 195 MB) are
/// pipelined.
pub const BAND_FLOOR_BYTES: u64 = 24 << 20;

/// Hard ceiling on the number of bands for one surface, so a pathological aspect ratio (a 200 MP
/// panorama one pixel tall would want thousands) cannot turn one readback into a submit storm.
pub const BAND_MAX: u32 = 64;

/// How a surface is cut: `rows` rows per band, `bands` bands. `bands == 1` is the WHOLE transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandPlan {
    /// Rows in every band but (possibly) the last.
    pub rows: u32,
    /// Total band count. 1 = monolithic.
    pub bands: u32,
}

impl BandPlan {
    /// The whole surface in one piece — the v0.8.165 shape.
    #[must_use]
    pub fn whole(h: u32) -> BandPlan {
        BandPlan { rows: h.max(1), bands: 1 }
    }

    /// Is this the monolithic (unsliced) plan?
    #[must_use]
    pub fn is_whole(&self) -> bool {
        self.bands <= 1
    }

    /// Band `k` as `(first_row, rows)`. The last band is short whenever `h` is not a multiple of
    /// [`BandPlan::rows`]; a `k` past the end answers `(h, 0)` so a caller's loop cannot read past
    /// the buffer even if it miscounts.
    #[must_use]
    pub fn band(&self, k: u32, h: u32) -> (u32, u32) {
        let first = self.rows.saturating_mul(k).min(h);
        (first, (h - first).min(self.rows))
    }
}

/// The plan for a `w × h` surface at `bpp` bytes per pixel, with every constant stated. The shipped
/// call is [`plan`]; this form exists so the tests can drive the boundaries without depending on
/// the shipped numbers (and so a future device-derived budget has one place to enter).
///
/// Degenerate inputs (a zero dimension, zero bpp) answer [`BandPlan::whole`] — the caller's
/// existing path — rather than dividing by zero.
#[must_use]
pub fn plan_with(w: u32, h: u32, bpp: u32, target: u64, floor: u64, max_bands: u32) -> BandPlan {
    if w == 0 || h == 0 || bpp == 0 || max_bands <= 1 {
        return BandPlan::whole(h);
    }
    let row = u64::from(w) * u64::from(bpp);
    let total = row * u64::from(h);
    if total <= floor {
        return BandPlan::whole(h);
    }
    // At least one row per band even when a single row is larger than the target: a row is the
    // indivisible unit of `write_texture`, so the target is a goal, not a guarantee.
    let mut rows = (target / row).clamp(1, u64::from(h)) as u32;
    let mut bands = h.div_ceil(rows);
    if bands > max_bands {
        rows = h.div_ceil(max_bands);
        bands = h.div_ceil(rows);
    }
    if bands <= 1 {
        BandPlan::whole(h)
    } else {
        BandPlan { rows, bands }
    }
}

/// The live band target. Written ONCE at boot by [`set_target_bytes`] from the adapter's own
/// budget; [`BAND_TARGET_BYTES`] until then. Read per plan so a call before boot finishes is
/// answered by the default rather than by a race.
static TARGET: AtomicU64 = AtomicU64::new(BAND_TARGET_BYTES);

/// v0.8.166 (iGPU deliverable) — narrow the band target on a SHARED-MEMORY adapter.
///
/// `budget` is the same number the texture caches size themselves from (`vram_budget_bytes`:
/// dedicated VRAM on a discrete card, `SharedSystemMemory/4` on an integrated one). Below 4 GiB the
/// target halves, so the transient host-visible staging a readback holds is two 8 MiB buffers = 16
/// MiB rather than 32 — and on an iGPU those bytes are system RAM competing with the app's own
/// working set. Worst-case sizing, not current-frame sizing, per the standing hardware lens.
///
/// v0.8.167: the one caller passes `Some(actual_vram)` — the boot's already-resolved budget, the
/// same value the caches were sized from — instead of a SECOND `vram_budget_bytes()` query whose
/// `None` on a failed DXGI call would have read here as "not a weak box" while the caches beside it
/// had fallen back to the conservative default. One reading, one decision (L33: name the instance).
///
/// L43 — WHAT UN-SETS THIS: nothing, and nothing needs to. It is written once from an immutable
/// property of the adapter, and it only ever moves the target to the SAFER (smaller) side; there is
/// no favourable posture here that could outlive its justification. A second call simply overwrites
/// with the same reasoning.
pub fn set_target_bytes(budget: Option<u64>) -> u64 {
    let t = match budget {
        Some(b) if b < (4 << 30) => BAND_TARGET_BYTES / 2,
        _ => BAND_TARGET_BYTES,
    };
    TARGET.store(t, Ordering::Relaxed);
    t
}

/// Is band-slicing armed this session? `FALCON_HEIC_READBACK_BANDS=0` reverts the HEIC assembly
/// readback to the whole-picture v0.8.165 shape — the A/B instrument for any measurement pass, and
/// the field escape hatch if a driver ever hates many small mapped copies. Parses exactly `=0`, the
/// `FALCON_HEIC_GPU_COLOR` / `hw_disabled_from_env` precedent: set to anything else is not a
/// disable. Default ON — the pipelined readback is the product.
///
/// v0.8.167: this lever REPLACES `FALCON_PUBLISH_BANDS`, which died with the renderer-side banding
/// the 08-06 audit reverted. The name is the honest one: it governs the readback and nothing else,
/// and a variable named for the publish would have promised an arm that no longer exists.
#[must_use]
pub fn armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        !disabled_from_env(std::env::var("FALCON_HEIC_READBACK_BANDS").ok().as_deref())
    })
}

/// The lever's parse, pure so it can be RUN rather than argued (the `gpu_color_disabled_from_env`
/// precedent). Exactly `=0` disables; anything else, including unset, leaves banding armed.
#[inline]
#[must_use]
pub fn disabled_from_env(v: Option<&str>) -> bool {
    v == Some("0")
}

/// [`plan_with`] against the live constants — the one the call site uses. Answers
/// [`BandPlan::whole`] for every surface while the lever is off, which is exactly the v0.8.165
/// readback.
#[must_use]
pub fn plan(w: u32, h: u32, bpp: u32) -> BandPlan {
    if !armed() {
        return BandPlan::whole(h);
    }
    plan_with(w, h, bpp, TARGET.load(Ordering::Relaxed), BAND_FLOOR_BYTES, BAND_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped plan with the shipped constants, INDEPENDENT of the process-wide lever and of
    /// whatever `set_target_bytes` a sibling test may have stored — these rows are about the
    /// arithmetic, and a test that reads a global would be measuring the test order.
    fn p(w: u32, h: u32, bpp: u32) -> BandPlan {
        plan_with(w, h, bpp, BAND_TARGET_BYTES, BAND_FLOOR_BYTES, BAND_MAX)
    }

    /// FALSIFIER: invert `disabled_from_env`'s comparison (or widen it to any set value) and this
    /// reddens — the lever must be exactly `=0`, so `FALCON_HEIC_READBACK_BANDS=1` cannot silently
    /// disable the thing it reads like it enables.
    #[test]
    fn the_lever_parses_exactly_zero() {
        assert!(disabled_from_env(Some("0")));
        assert!(!disabled_from_env(None));
        assert!(!disabled_from_env(Some("1")));
        assert!(!disabled_from_env(Some("")));
        assert!(!disabled_from_env(Some("00")));
        assert!(!disabled_from_env(Some("false")));
    }

    /// FALSIFIER: make `set_target_bytes` ignore its argument (return `BAND_TARGET_BYTES` always)
    /// and the weak-tier assert reddens. The band count assert is the DIFFERENTIAL: the same 48 MP
    /// readback must slice FINER on the shared-memory budget than on the discrete one, which is the
    /// whole point of consulting the budget at all.
    #[test]
    fn a_shared_memory_budget_narrows_the_band() {
        let big = set_target_bytes(Some(16 << 30)); // a 16 GB discrete card
        assert_eq!(big, BAND_TARGET_BYTES);
        let small = set_target_bytes(Some(2 << 30)); // an iGPU: SharedSystemMemory/4
        assert_eq!(small, BAND_TARGET_BYTES / 2);
        assert_eq!(set_target_bytes(None), BAND_TARGET_BYTES, "an unknown budget is not a weak one");
        let wide = plan_with(8064, 6048, 4, big, BAND_FLOOR_BYTES, BAND_MAX);
        let tight = plan_with(8064, 6048, 4, small, BAND_FLOOR_BYTES, BAND_MAX);
        assert!(
            tight.bands > wide.bands,
            "the shared-memory budget must slice finer: {tight:?} vs {wide:?}"
        );
        // …and restore the default so no sibling test reads a narrowed global.
        set_target_bytes(None);
    }

    /// FALSIFIER: change `BandPlan::whole`'s `bands` to 2, or drop the `total <= floor` early
    /// return in `plan_with`, and this reddens — a small readback would start paying for
    /// scheduling it does not need.
    #[test]
    fn small_surfaces_stay_whole() {
        // A fast-tier HEIC finish: 1920 × 1280 at one word per pixel ≈ 9.8 MB.
        let plan0 = p(1920, 1280, 4);
        assert!(plan0.is_whole(), "a 9.8 MB readback must stay whole, got {plan0:?}");
        assert_eq!(plan0.bands, 1);
        // Exactly at the floor is still whole (the boundary is `<=`).
        let rows = (BAND_FLOOR_BYTES / (1024 * 4)) as u32;
        assert!(p(1024, rows, 4).is_whole(), "a transfer exactly at the floor must stay whole");
        // One row over it is not.
        assert!(!p(1024, rows + 1, 4).is_whole(), "one row past the floor must slice");
    }

    /// FALSIFIER: raise `BAND_TARGET_BYTES` above the picture size, or make `plan_with` return
    /// `whole` for large surfaces, and the band-count assert reddens. This is the row that pins the
    /// headline number — a 48 MP readback is 12 bands, and every band is ≤ the target.
    #[test]
    fn a_48mp_readback_slices_into_target_sized_bands() {
        let (w, h) = (8064u32, 6048u32);
        let pl = p(w, h, 4);
        assert!(!pl.is_whole());
        assert_eq!(pl.bands, 12, "48 MP at a 16 MiB target is 12 bands, got {pl:?}");
        let band_bytes = u64::from(pl.rows) * u64::from(w) * 4;
        assert!(
            band_bytes <= BAND_TARGET_BYTES,
            "a band carries {band_bytes} B, over the {BAND_TARGET_BYTES} B target"
        );
    }

    /// FALSIFIER: make `BandPlan::band` return `self.rows` unconditionally (i.e. drop the
    /// `(h - first).min(..)` clamp) and the total-coverage assert reddens on any `h` that is not a
    /// multiple of `rows` — which is the out-of-bounds copy the readback would then issue. This is
    /// the DIFFERENTIAL row for "the bands are a partition": every row covered exactly once, no
    /// row covered twice, none past the end (L28 sub-clause).
    #[test]
    fn bands_partition_every_row_exactly_once() {
        for (w, h, bpp) in [(8064u32, 6048u32, 4u32), (3840, 2560, 4), (999, 4001, 4), (8064, 1, 4), (4, 1_000_000, 1)] {
            let pl = p(w, h, bpp);
            let mut next = 0u32;
            let mut seen = 0u64;
            for k in 0..pl.bands {
                let (first, rows) = pl.band(k, h);
                assert_eq!(first, next, "band {k} of {pl:?} starts at {first}, expected {next}");
                assert!(rows > 0, "band {k} of {pl:?} is empty");
                next = first + rows;
                seen += u64::from(rows);
            }
            assert_eq!(next, h, "{pl:?} covers up to row {next} of {h}");
            assert_eq!(seen, u64::from(h), "{pl:?} covers {seen} rows of {h}");
            assert!(pl.bands <= BAND_MAX, "{pl:?} exceeds the {BAND_MAX} band ceiling");
            // …and a band past the end is empty rather than out of range.
            assert_eq!(pl.band(pl.bands, h), (h, 0));
        }
    }

    /// FALSIFIER: delete the `bands > max_bands` re-derivation in `plan_with` and this reddens on
    /// the WIDE case — a surface whose single ROW is bigger than the band target gets one row per
    /// band, so a 100-row one would want 100 submits and a 4000-row one 4000. The ceiling is what
    /// stops one readback becoming a submit storm; the clamp then re-derives `rows` so the bands
    /// still partition the surface (the partition row above covers this shape too).
    #[test]
    fn the_band_ceiling_binds_on_pathological_shapes() {
        // One row = 32 MB, twice the target ⇒ the unclamped plan is one row per band.
        let pl = p(8_000_000, 400, 4);
        assert!(pl.bands <= BAND_MAX, "{pl:?} must respect the {BAND_MAX} ceiling");
        assert!(pl.bands > 1, "a 12.8 GB surface still slices, got {pl:?}");
        assert_eq!(pl.rows, 400u32.div_ceil(BAND_MAX), "the clamp re-derives rows from the ceiling");
        // And the tall-thin shape whose TOTAL is under the floor is simply left whole — the cheap
        // path, not a ceiling case.
        assert!(p(4, 1_000_000, 1).is_whole(), "a 4 MB surface is under the floor");
    }

    /// Degenerate inputs must reach the caller's existing whole-surface path rather than produce a
    /// plan nobody can execute.
    ///
    /// FALSIFIER (CORRECTED v0.8.167 — the previous one was false): change `plan_with`'s
    /// `max_bands <= 1` term to `max_bands == 0` and the LAST assert reddens, because a ceiling of
    /// one would then fall through to the divide-and-clamp arithmetic. What this row does NOT pin,
    /// despite what its doc claimed until v0.8.167, is the `w == 0 || h == 0 || bpp == 0` guard:
    /// with any zero among the three the ROW SIZE is 0, so `total` is 0, so the `total <= floor`
    /// early return answers `whole` before any division can happen — even at the `floor = 0` the
    /// HEIC parity gate passes. Removing that guard changes no answer here and panics nothing. It
    /// is a belt; the doc called it the buckle.
    #[test]
    fn degenerate_dims_answer_whole() {
        assert!(p(0, 6048, 4).is_whole());
        assert!(p(8064, 0, 4).is_whole());
        assert!(p(8064, 6048, 0).is_whole());
        assert!(plan_with(8064, 6048, 4, BAND_TARGET_BYTES, BAND_FLOOR_BYTES, 1).is_whole());
    }
}
