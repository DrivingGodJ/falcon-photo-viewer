//! v0.9.23 (the ONE-POOL round, logic.md §5 item 2 — owner-approved): the macOS elastic
//! decode-pool GOVERNOR, as PURE platform-neutral state machines.
//!
//! On macOS the fixed decode(10)/thumbnail(3) pools are replaced by ONE elastic pool whose width
//! lives between a SMOOTHNESS FLOOR and a PRESSURE-GOVERNED CEILING:
//!
//!   • FLOOR = ceil(r × L): r = the applied scrub-fps target, L = the measured per-decode serial
//!     latency at the applied scrub tier — BOTH from the persisted benchmark record, never
//!     constants (Little's law: r×L concurrent decodes sustain r deliveries/s at latency L).
//!     r is additionally clamped to the tier's persisted browse-sustained rate, so a "Max" (120)
//!     scrub target derives the floor from what the machine can actually deliver, not a wish.
//!     Missing/incomplete bench → a conservative floor of 3 (logged honestly at boot).
//!     Sanity anchor (the M4 Pro round-3 field data): 20 fps × ~148 ms ≈ 3, and the measured
//!     posture matrix's width 3 = 19.5 fps confirms the formula.
//!   • CEILING = the P-core count (hw.perflevel0.logicalcpu — 8 on the tester's M4 Pro, whose
//!     matrix showed width 8 = 51.9 fps healthy and width 10 = latency inflation). The E-cores
//!     implicitly stay free for the UI/upload/OS. No perflevel sysctl (Intel Mac) → cores − 2.
//!   • The governor GROWS +1 toward the ceiling when the scrub is STARVED (delivered fps < target
//!     with queue depth) and SHRINKS −1 toward the floor under RAM pressure — the pressure input
//!     is [`crate::l2::pressure_zone`], the L2 valve's OWN zone constants, so the valve's degrade
//!     signal IS the governor's shrink signal by construction.
//!   • Hysteresis so it never oscillates: each grow needs [`POOL_GROW_STREAK`] consecutive 1 Hz
//!     starved+Calm samples; each shrink needs [`POOL_SHRINK_STREAK`] consecutive Low samples
//!     (safety reacts faster than ambition). The Dead band between Low and Calm resets BOTH
//!     streaks — level hysteresis exactly like the valve's. Decisions are ≤ 1/s by construction
//!     (the tick samples at 1 Hz), which is also the decision-log rate limit.
//!
//! EVERYTHING here is platform-neutral and Windows-runnable under `cargo test` (the aarch64 target
//! is compile-checked but its `#[cfg(test)]` code is compiler-blind — the standing rule: logic in
//! the pure fns). The macOS thread code (main.rs `spawn_elastic_pool` + tick.rs
//! `step_pool_governor`) only EXECUTES the decisions made here. Windows never constructs any of
//! this — its fixed pools are byte-identical (the round's hard gate).
//!
//! Worker-count changes ([`ElasticGate`]): grow = spawn the returned dead slots; shrink = a worker
//! observes `slot ≥ target` between jobs and exits at its next dequeue — a running decode is NEVER
//! interrupted, and a shrunk-away job queue is untouched (drained by the survivors; proven by the
//! threaded drain test below).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))] // the menubar_model precedent: Windows compiles + TESTS this, only macOS constructs it

use crate::*;

/// Conservative fallback floor when the persisted bench record is missing/incomplete (fresh
/// install, schema mismatch, or a pre-v0.9.23 record without the serial-latency fields).
pub(crate) const POOL_CONSERVATIVE_FLOOR: usize = 3;
/// Consecutive starved+Calm 1 Hz samples required per +1 grow step: ~3 s of a REAL sustained
/// scrub rhythm, so a single settle blip or one slow frame never widens the pool.
pub(crate) const POOL_GROW_STREAK: u32 = 3;
/// Consecutive Low-zone 1 Hz samples per −1 shrink step: ~2 s. Deliberately shorter than the grow
/// streak (memory safety reacts faster than throughput ambition), and the shrink zone sits a full
/// 1.5 GiB restore band below the grow zone (l2.rs), so grow/shrink can never chase each other.
pub(crate) const POOL_SHRINK_STREAK: u32 = 2;

// ── floor / ceiling derivation (boot-time, pure) ─────────────────────────────────────────

/// The persisted-bench inputs the floor derives from. Built by [`FloorBench::from_settings`], which
/// owns the schema gate, the tier pick AND the provenance — the three facts the boot site used to
/// re-derive inline, where D-O2 found only the first of them present. All three numbers must be > 0
/// to count as a complete record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FloorBench {
    /// The APPLIED scrub-fps target (Settings `scrub_fps` — what the user's speed stop asks for).
    pub(crate) applied_fps: f32,
    /// The persisted browse-sustained rate at the APPLIED quality tier (bench_sub_fps or
    /// bench_super_fps) — clamps `applied_fps` so a Max(120) target derives from reality.
    pub(crate) sustained_fps: f32,
    /// The persisted serial per-decode latency (ms) at the APPLIED quality tier
    /// (bench_sub_lat_ms / bench_super_lat_ms — measured by the §43 bench since v0.9.23).
    pub(crate) lat_ms: f32,
    /// v0.9.62 (B2.3 / D-O2): did the record come from a SYNTHETIC run? The floor is L31's
    /// behavioural consumer — r×L sizes the pool, and W2-3 made that floor the resting width — so
    /// the provenance has to travel with the numbers. It does NOT refuse the record (see
    /// [`derive_floor`]); it changes what the boot line is allowed to claim about it.
    pub(crate) synthetic: bool,
}

impl FloorBench {
    /// v0.9.62 (B2.3): THE typed accessor. The schema gate, the tier pick and the provenance in one
    /// place, so no call site can take two of the three and forget the last — which is exactly what
    /// v0.9.23's inline `(boot.bench_ver == BENCH_SCHEMA_VER).then(|| …)` did.
    /// `None` = no record this boot may derive anything from.
    pub(crate) fn from_settings(s: &Settings) -> Option<FloorBench> {
        if s.bench_ver != crate::BENCH_SCHEMA_VER {
            return None;
        }
        // The APPLIED quality tier — the scrub the floor must keep smooth is the one that runs.
        let (sustained_fps, lat_ms) = if s.quality_super {
            (s.bench_super_fps as f32, s.bench_super_lat_ms)
        } else {
            (s.bench_sub_fps as f32, s.bench_sub_lat_ms)
        };
        Some(FloorBench {
            applied_fps: s.scrub_fps,
            sustained_fps,
            lat_ms,
            synthetic: s.bench_synthetic,
        })
    }
}

/// The derived floor + the honest provenance for the boot log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FloorPlan {
    /// floor = max(conservative, ceil(r_eff × lat)) clamped to [2, cores], from a run that measured
    /// the folder in front of the user; the fields echo the inputs for the log.
    FromBench { floor: usize, r_eff: f32, lat_ms: f32 },
    /// v0.9.62 (B2.3): the same arithmetic, from a SYNTHETIC run — generated frames of a chosen
    /// megapixel count, on the internal disk, warm. That is a real measurement of a MACHINE and no
    /// measurement at all of a lane, so the floor it derives is a machine LOWER BOUND. It is used
    /// rather than refused, deliberately: on the round-4 tester's numbers refusing would have taken
    /// the floor from 5 to the conservative 3 against a true ~11, and W2-3 made the floor the pool's
    /// resting width — refusal would have regressed the browse to protect a sentence. The
    /// difference this arm makes is that the boot line SAYS which of the two it is.
    FromSyntheticBench { floor: usize, r_eff: f32, lat_ms: f32 },
    /// No usable bench record → the conservative constant (clamped to [2, cores]).
    Conservative { floor: usize },
}
impl FloorPlan {
    pub(crate) fn floor(&self) -> usize {
        match *self {
            FloorPlan::FromBench { floor, .. }
            | FloorPlan::FromSyntheticBench { floor, .. }
            | FloorPlan::Conservative { floor } => floor,
        }
    }
}

/// The smoothness floor. `bench` is `None` when the persisted record is absent/incomplete
/// (schema mismatch, no run yet, pre-latency-era record) — any non-positive field also degrades
/// to the conservative arm, so a partially-written record can never derive a floor of 0.
///
/// v0.9.62 (B2.3): the derived value is `max(POOL_CONSERVATIVE_FLOOR, ceil(r×L))` before the
/// [2, cores] clamp. The conservative constant is a SMOOTHNESS FLOOR, not a fallback: a machine that
/// derives 1 or 2 is not a machine that wants a 1- or 2-wide pool, it is a machine whose measured
/// lane happens to be cheap, and W2-3's idle shrink already returns the pool to this width at rest
/// (a parked worker costs ~nothing since the condvar park landed). Platform-neutral and fully
/// testable on the Windows host — only macOS ever CONSTRUCTS any of this.
pub(crate) fn derive_floor(bench: Option<FloorBench>, cores: usize) -> FloorPlan {
    let lo = 2usize;
    let hi = cores.max(lo);
    match bench {
        Some(b) if b.applied_fps > 0.0 && b.sustained_fps > 0.0 && b.lat_ms > 0.0 => {
            let r_eff = b.applied_fps.min(b.sustained_fps);
            let derived = (r_eff * b.lat_ms / 1000.0).ceil() as usize;
            let floor = derived.max(POOL_CONSERVATIVE_FLOOR).clamp(lo, hi);
            if b.synthetic {
                FloorPlan::FromSyntheticBench { floor, r_eff, lat_ms: b.lat_ms }
            } else {
                FloorPlan::FromBench { floor, r_eff, lat_ms: b.lat_ms }
            }
        }
        _ => FloorPlan::Conservative { floor: POOL_CONSERVATIVE_FLOOR.clamp(lo, hi) },
    }
}

/// The pressure-governed ceiling's START value: the P-core count when the perflevel sysctl gave a
/// sane answer (1..=cores), else cores − 2 (an Intel Mac has no perflevel sysctls and every core
/// is a P-core — reserve the UI+upload pair, the pool18 discipline). Never below the floor: the
/// smoothness floor is a FLOOR by definition (perf + smooth UX are the two product foundations),
/// so a floor above P-count wins — flagged in the round report as the resolution of that corner.
pub(crate) fn derive_ceiling(p_cores: Option<u32>, cores: usize, floor: usize) -> usize {
    let base = match p_cores {
        Some(p) if p >= 1 && (p as usize) <= cores => p as usize,
        _ => cores.saturating_sub(2).max(2),
    };
    base.max(floor)
}

// ── the 1 Hz decision state machine (pure) ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub(crate) struct GovParams {
    pub(crate) floor: usize,
    pub(crate) ceiling: usize,
}

/// v0.9.60 (W2-3 / findings energy item 3): consecutive IDLE samples before the width falls back
/// to the floor. The round-4 log ratcheted 4→5→6→7→8 and STAYED at 8 for the rest of the session,
/// because the only shrink arm needs `PressureZone::Low` and 18 GB free on 24 never gets there:
/// width was a one-way ratchet, and on a hardware-decoder lane the extra slots bought zero
/// delivered fps — four P-cores of pure heat. Ten seconds with an empty queue AND no completion of
/// either job class is not a browse pause: it is a photographer who has stopped, and the floor is
/// by construction the width that keeps their applied scrub smooth, so returning to it is
/// returning to the designed state rather than degrading below it.
///
/// v0.9.61 (A3) — THE §43 QUESTION, ANSWERED AND RECORDED. The round-4 audit asked whether an idle
/// shrink could hurt a warm zoomed browse: the §43 always-sharp path keeps decoding at full
/// resolution, so does taking the pool back to its floor starve it? No. §43's full-res decodes run
/// on the DETAIL thread — a dedicated, separately-spawned thread this elastic pool never serves —
/// and the pool's two job classes (fast tier, thumbnails) are exactly the two this sample counts.
/// A window with no queued fast work, nothing in flight, no fast frame and no thumbnail IS an idle
/// pool even while a detail decode runs, and returning to the floor is correct there. What was
/// wrong was only the v0.9.60 log SENTENCE, which named two of the tested terms and implied the
/// others; it names all four now.
pub(crate) const POOL_IDLE_SHRINK_SAMPLES: u32 = 10;

/// The governor's mutable state between samples. `width` is authoritative (single writer — the
/// UI-thread tick); the [`ElasticGate`] target mirrors it after each applied decision.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GovState {
    pub(crate) width: usize,
    grow_streak: u32,
    shrink_streak: u32,
    /// v0.9.60 (W2-3): consecutive samples in which the pool did nothing at all.
    idle_streak: u32,
}
impl GovState {
    pub(crate) fn new(width: usize) -> GovState {
        GovState { width, grow_streak: 0, shrink_streak: 0, idle_streak: 0 }
    }
}

/// One 1 Hz observation. `delivered_fps` = fast-tier decode completions/s over the sample window
/// (with the Mac L2 off, completions ARE deliveries); `target_fps` = the LIVE applied scrub-fps;
/// `queue_depth` = the pump queue length at sample time (unmet demand); `zone` = the valve's
/// avail-RAM zone from the SAME probe family the L2 controller steps on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GovSample {
    pub(crate) delivered_fps: f32,
    pub(crate) target_fps: f32,
    pub(crate) queue_depth: usize,
    pub(crate) zone: crate::l2::PressureZone,
    /// v0.9.60 (W2-3): thumbnail jobs completed in the window. The elastic pool serves TWO job
    /// classes and `delivered_fps` counts only the fast one, so this is what stops a filmstrip
    /// scroll from being classified as an idle pool.
    pub(crate) thumb_done: usize,
    /// v0.9.61 (A3 / J15): fast decodes RUNNING at sample time — `Pump.inflight.len()`, read in the
    /// same lock that already reads `queue_depth`. Without it the idle test was blind to the exact
    /// state a long decode produces: the queue is empty because the job has been POPPED, and the
    /// window delivered nothing because the decode has not finished yet. Ten such samples in a row
    /// is a slow 48 MP RAW, not an idle pool, and shrinking under it takes workers away from the
    /// very work that is running.
    pub(crate) inflight: usize,
    /// v1.0 MERGE (F1-2 / F1's C1): IS THE FAST TIER'S DISPATCH HOLD IN FORCE THIS WINDOW?
    ///
    /// The merge puts two schedulers that were each designed as the only one on the same machine.
    /// Trunk's W4 ladder decides WHETHER speculative work is dispatched; this governor decides HOW
    /// MANY workers exist. They share exactly one channel — the pump queue — and they read it with
    /// OPPOSITE meanings: `step_prefetch_fast` EMPTIES it on purpose for the length of a gesture
    /// (`support::fast_speculation_held`), and `pool_idle` reads an empty queue as evidence the pool
    /// is over-provisioned. Post-merge, a continuous zoom-drag on a cached frame therefore produced
    /// ten consecutive `queue 0 / inflight 0 / delivered 0 / thumbs 0` samples and shrank an
    /// eight-wide pool to the conservative floor MID-GESTURE — and the whole held want set plus the
    /// runway then landed on three workers at release, ~15 s from full width again.
    ///
    /// A HELD POOL IS SUPPRESSED, NOT IDLE. That is the whole term, and it goes in the SAMPLE rather
    /// than in a constant for the reason F1's C1 gives: the governor has to take the gate as an
    /// input, exactly as it already takes `thumb_done` and `inflight`.
    pub(crate) held: bool,
}

/// v0.9.61 (A3 / J19): WHY a shrink happened. The two shrink paths reach the same `resize` and used
/// to reach the same log line, which then asserted RAM pressure for both — a line describing a cause
/// the code had not tested (L30). Carried on the decision so the log states the reason the state
/// machine actually took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShrinkCause {
    /// The pressure table: [`POOL_SHRINK_STREAK`] consecutive `Low`-zone samples, one step down.
    Pressure,
    /// The idle clock: [`POOL_IDLE_SHRINK_SAMPLES`] consecutive samples with nothing queued,
    /// nothing in flight, no fast frame delivered and no thumbnail finished — straight to the floor.
    Idle,
}

/// What the thread side must DO (and log). At most one step per sample — decisions are ≤ 1/s by
/// construction, which is also the decision-line log rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GovDecision {
    Hold,
    Grow { from: usize, to: usize },
    Shrink { from: usize, to: usize, cause: ShrinkCause },
}

/// Starvation: demand is outstanding AND delivery misses the live target. The grow streak (3 s)
/// is the jitter guard — a 19.9-vs-20 boundary flicker can't sustain three consecutive samples
/// unless the scrub genuinely runs starved, in which case growing is exactly right.
fn starved(s: &GovSample) -> bool {
    s.queue_depth > 0 && s.delivered_fps < s.target_fps
}

/// v0.9.60 (W2-3): did this pool do NOTHING for a whole sample window? Nothing queued, nothing in
/// flight, no fast frame delivered, no thumbnail finished — all four, because any one of them alone
/// is a state the pool legitimately sits in while working (an empty queue with frames still landing
/// is a well-fed pool; zero fast frames with thumbnails flowing is a filmstrip scroll; and — v0.9.61
/// (A3/J15) — an empty queue with zero deliveries is ALSO what a decode that has been popped but has
/// not finished looks like, which is precisely the sample a slow shot produces).
///
/// v1.0 MERGE (F1-2): — and FIVE, because the trunk's fast-tier dispatch hold empties the very queue
/// this test reads. See [`GovSample::held`] for the whole argument.
fn pool_idle(s: &GovSample) -> bool {
    !s.held
        && s.queue_depth == 0
        && s.inflight == 0
        && s.delivered_fps <= 0.0
        && s.thumb_done == 0
}

/// The whole decision table (every arm exercised by the tests below):
///
/// | zone | starved | effect |
/// |------|---------|--------|
/// | Low  |  (any)  | grow_streak=0; shrink_streak+1 → at [`POOL_SHRINK_STREAK`]: −1 (≥ floor) |
/// | Dead |  (any)  | BOTH streaks = 0; Hold (the valve's level-hysteresis band, mirrored)     |
/// | Calm |   yes   | shrink_streak=0; grow_streak+1 → at [`POOL_GROW_STREAK`]: +1 (≤ ceiling) |
/// | Calm |   no    | both streaks = 0; Hold                                                   |
///
/// At the floor, Low samples keep the streak pinned but never step below; at the ceiling,
/// starved+Calm samples likewise hold. Each applied step resets its own streak (a full fresh
/// streak per step — the valve's restore discipline).
///
/// v0.9.60 (W2-3) adds ONE arm AFTER that table, never inside it: when the table decides `Hold`
/// and the pool has been [`pool_idle`] for [`POOL_IDLE_SHRINK_SAMPLES`] consecutive samples, the
/// width drops STRAIGHT to the floor. Ordering matters and is deliberate — every pre-existing arm
/// keeps its exact behaviour and its tests, and the idle arm can only act where the old code did
/// nothing at all.
pub(crate) fn govern(st: &mut GovState, p: &GovParams, s: &GovSample) -> GovDecision {
    let decision = govern_pressure(st, p, s);
    if !pool_idle(s) {
        st.idle_streak = 0;
        return decision;
    }
    st.idle_streak = st.idle_streak.saturating_add(1);
    if decision != GovDecision::Hold {
        return decision; // a real pressure/starvation step outranks the idle clock
    }
    if st.idle_streak >= POOL_IDLE_SHRINK_SAMPLES && st.width > p.floor {
        st.idle_streak = 0; // a full fresh streak per step (the valve's restore discipline)
        let from = st.width;
        st.width = p.floor;
        return GovDecision::Shrink { from, to: st.width, cause: ShrinkCause::Idle };
    }
    GovDecision::Hold
}

/// The pressure/starvation table above, verbatim — extracted so the idle arm can compose with it
/// instead of editing it.
fn govern_pressure(st: &mut GovState, p: &GovParams, s: &GovSample) -> GovDecision {
    use crate::l2::PressureZone::*;
    match s.zone {
        Low => {
            st.grow_streak = 0;
            st.shrink_streak += 1;
            if st.shrink_streak >= POOL_SHRINK_STREAK && st.width > p.floor {
                st.shrink_streak = 0;
                let from = st.width;
                st.width -= 1;
                return GovDecision::Shrink { from, to: st.width, cause: ShrinkCause::Pressure };
            }
            GovDecision::Hold
        }
        Dead => {
            st.grow_streak = 0;
            st.shrink_streak = 0;
            GovDecision::Hold
        }
        Calm => {
            st.shrink_streak = 0;
            if starved(s) {
                st.grow_streak += 1;
                if st.grow_streak >= POOL_GROW_STREAK && st.width < p.ceiling {
                    st.grow_streak = 0;
                    let from = st.width;
                    st.width += 1;
                    return GovDecision::Grow { from, to: st.width };
                }
            } else {
                st.grow_streak = 0;
            }
            GovDecision::Hold
        }
    }
}

// ── the worker-width protocol (shared-state, thread-safe, Windows-testable) ──────────────

/// The elastic pool's width gate: a target width + a per-slot alive bitmap under ONE mutex, so a
/// worker's exit decision and the governor's respawn decision serialize — the lost-slot race
/// (worker decides to exit, governor grows before it clears its slot) is structurally impossible:
/// either the worker already cleared `alive[slot]` (the resize returns the slot to spawn) or it
/// hasn't (it re-reads the RAISED target at this same check and stays).
///
/// Workers call [`ElasticGate::worker_should_exit`] BETWEEN jobs only (never mid-decode); the
/// governor applies each [`GovDecision`] via [`ElasticGate::resize`] and then notifies the pump
/// condvar so parked workers re-check. Queue contents are never touched by either side — no task
/// is ever lost on a shrink (the survivors drain it; see `elastic_gate_no_task_loss_under_resize`).
///
/// v0.9.61 (A3 / J20): [`ElasticGate::worker_should_exit`] MUTATES — it marks the slot dead under
/// the lock, which is exactly right at the one site that then exits, and exactly wrong anywhere it
/// is used as a question. The park's pre-sleep re-check asked it as a question, so a worker that was
/// about to sleep could mark its own slot dead and then park anyway: the gate's documented invariant
/// ("either the worker cleared alive[slot], or it re-reads the raised target and stays") had a third
/// case, and the next grow would spawn a SECOND worker for a slot that still had one. The re-check
/// now reads [`ElasticGate::slot_condemned`] — a lock-free load of `target_hint`, published inside
/// `resize` under the gate mutex — which also DELETES the PUMP→GATE lock nesting that re-check
/// created rather than documenting it.
pub(crate) struct ElasticGate {
    state: Mutex<GateState>,
    /// A lock-free mirror of `state.target`, stored INSIDE `resize` while the gate mutex is held.
    /// Readers get a value that is either the current target or the immediately previous one, which
    /// is all a pre-sleep hint has to be: a stale "not condemned" costs one more loop iteration (the
    /// worker re-reads under the real lock at the top of the loop), and a stale "condemned" costs a
    /// wake that the authoritative `worker_should_exit` then declines.
    target_hint: AtomicUsize,
}
struct GateState {
    target: usize,
    alive: Vec<bool>,
}
impl ElasticGate {
    /// `initial` slots 0..initial are considered alive (the boot spawn); capacity = `ceiling`.
    pub(crate) fn new(initial: usize, ceiling: usize) -> ElasticGate {
        let cap = ceiling.max(initial);
        let mut alive = vec![false; cap];
        for a in alive.iter_mut().take(initial) {
            *a = true;
        }
        ElasticGate {
            state: Mutex::new(GateState { target: initial, alive }),
            target_hint: AtomicUsize::new(initial),
        }
    }
    /// The NON-mutating twin of [`worker_should_exit`]: "is this slot at or above the target?" Reads
    /// the lock-free hint, so it is safe to ask while holding the PUMP mutex — which is the whole
    /// point, since the only caller is the park's pre-sleep re-check. Never touches `alive`.
    pub(crate) fn slot_condemned(&self, slot: usize) -> bool {
        slot >= self.target_hint.load(Ordering::Acquire)
    }
    /// The governor's current WIDTH. v0.8.114 (V12): this is a PRODUCTION read on this trunk,
    /// not test-harness observability — the tick shadows `support::costly_prefetch_cap` off it
    /// every tick so "a quarter of the pool" stays true as the elastic pool breathes (see the
    /// `#[cfg(target_os = "macos")] let costly_cap` shadow in `main.rs`). The threaded drain test
    /// also asserts convergence through it, which is where the old `#[allow(dead_code)]` came
    /// from; that annotation was false from the moment the shadow landed.
    pub(crate) fn target(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).target
    }
    #[allow(dead_code)] // test-harness observability
    pub(crate) fn alive_count(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).alive.iter().filter(|a| **a).count()
    }
    /// Worker-side, between jobs: true ⇒ this worker must return NOW (its slot is already marked
    /// dead under the lock — the caller just exits its loop).
    pub(crate) fn worker_should_exit(&self, slot: usize) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if slot >= st.target {
            if let Some(a) = st.alive.get_mut(slot) {
                *a = false;
            }
            return true;
        }
        false
    }
    /// Governor-side: set the new target and return the slots that need a SPAWN (dead slots below
    /// the new target, marked alive here under the lock so a double-resize can't double-spawn).
    /// A shrink returns an empty vec — workers exit lazily at their next `worker_should_exit`.
    pub(crate) fn resize(&self, new_target: usize) -> Vec<usize> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let cap = st.alive.len();
        st.target = new_target.min(cap);
        let target = st.target;
        // v0.9.61 (A3): publish the hint UNDER the gate mutex, so a park re-check can read the
        // width without taking this lock while holding the pump's.
        self.target_hint.store(target, Ordering::Release);
        let mut spawn = Vec::new();
        for slot in 0..target {
            if !st.alive[slot] {
                st.alive[slot] = true;
                spawn.push(slot);
            }
        }
        spawn
    }
    /// Governor-side: a spawn ATTEMPT for `slot` FAILED (the OS refused the worker thread). Clear
    /// the slot back to dead under the SAME lock so a later [`resize`] re-offers it. `resize`
    /// commits `alive[slot]=true` BEFORE the fallible spawn (its double-spawn guard), so without
    /// this a failed spawn would strand a live slot with no worker AND the guard would never
    /// respawn it — a permanently phantom slot desyncing the width (the v0.9.23 addendum fix). The
    /// `target` is deliberately left untouched: the governor rolls its OWN width back to match, and
    /// the next grow re-offers this reopened slot. Out-of-range slots are a no-op (never panics).
    pub(crate) fn mark_dead(&self, slot: usize) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = st.alive.get_mut(slot) {
            *a = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2::PressureZone::{Calm, Dead, Low};

    // ── floor derivation ─────────────────────────────────────────────────────────────────

    fn bench(applied: f32, sustained: f32, lat: f32) -> Option<FloorBench> {
        Some(FloorBench {
            applied_fps: applied,
            sustained_fps: sustained,
            lat_ms: lat,
            synthetic: false,
        })
    }
    /// The same record, from a SYNTHETIC run (v0.9.62 / B2.3).
    fn synth(applied: f32, sustained: f32, lat: f32) -> Option<FloorBench> {
        bench(applied, sustained, lat).map(|b| FloorBench { synthetic: true, ..b })
    }

    #[test]
    fn floor_anchor_m4pro() {
        // The round's sanity anchor: the tester's M4 Pro — 20 fps applied, sustained well above,
        // ~148 ms serial latency at the scrub tier → ceil(20 × 0.148) = ceil(2.96) = 3.
        let plan = derive_floor(bench(20.0, 42.0, 148.0), 12);
        assert_eq!(plan, FloorPlan::FromBench { floor: 3, r_eff: 20.0, lat_ms: 148.0 });
        // Width 3 = 19.5 fps in their measured matrix — the formula's own confirmation: 3/19.5 ≈ 154 ms.
        let plan = derive_floor(bench(20.0, 42.0, 154.0), 12);
        assert_eq!(plan.floor(), 4, "154 ms tips ceil(3.08) to 4 — the formula is honest, not tuned");
    }

    #[test]
    fn floor_clamps_r_to_sustained() {
        // "Max" (120 fps) applied on a machine sustaining 42: r_eff = 42 → ceil(42 × 0.154) = 7.
        let plan = derive_floor(bench(120.0, 42.0, 154.0), 12);
        match plan {
            FloorPlan::FromBench { floor, r_eff, .. } => {
                assert_eq!(floor, 7);
                assert_eq!(r_eff, 42.0);
            }
            other => panic!("expected FromBench, got {other:?}"),
        }
    }

    /// v0.9.62 (B2.3): the derived floor may never come out BELOW the conservative constant. That
    /// constant is a smoothness floor, not a fallback — a cheap measured lane is not a reason to run
    /// a 1- or 2-wide pool, and the idle shrink already parks the extra workers for ~nothing.
    /// FALSIFIER (assert granularity): drop the `.max(POOL_CONSERVATIVE_FLOOR)` in `derive_floor` →
    /// the `"a cheap lane still gets the smoothness floor"` assert reddens with 2; drop the
    /// `.clamp(lo, hi)` → the `"…and a 2-core machine still caps at its cores"` assert reddens.
    #[test]
    fn floor_never_falls_below_the_conservative_smoothness_floor() {
        assert_eq!(
            derive_floor(bench(5.0, 60.0, 30.0), 12).floor(),
            POOL_CONSERVATIVE_FLOOR,
            "a cheap lane still gets the smoothness floor" // ceil(0.15) = 1 → 3
        );
        assert_eq!(
            derive_floor(bench(5.0, 60.0, 30.0), 2).floor(),
            2,
            "…and a 2-core machine still caps at its cores"
        );
        // Huge demand caps at cores.
        assert_eq!(derive_floor(bench(120.0, 120.0, 200.0), 8).floor(), 8); // ceil(24) → 8
    }

    /// v0.9.62 (B2.3 / D-O2): a SYNTHETIC record still derives a floor — refusing it would drop the
    /// pool BELOW what the machine demonstrably sustains (the skeptics' numbers: conservative 3 <
    /// synthetic 5 < true ~11) — but it lands in its own arm, so the boot line can say the number is
    /// a machine lower bound rather than this folder's lane.
    /// FALSIFIER (assert granularity): make `derive_floor` ignore `b.synthetic` (always `FromBench`)
    /// → the `"a synthetic record is named, not silently trusted"` assert reddens; make it refuse
    /// (fall through to `Conservative`) → the `"…and it is USED: 5, not the conservative 3"` assert
    /// reddens with 3, which is the browse regression the ruling forbids.
    #[test]
    fn a_synthetic_record_derives_the_floor_but_says_so() {
        let plan = derive_floor(synth(20.0, 42.0, 240.0), 12);
        assert!(
            matches!(plan, FloorPlan::FromSyntheticBench { .. }),
            "a synthetic record is named, not silently trusted — got {plan:?}"
        );
        assert_eq!(plan.floor(), 5, "…and it is USED: 5, not the conservative 3");
        // The folder-sourced twin of the same numbers is the OTHER arm with the SAME floor: the
        // provenance changes what may be claimed, never the arithmetic.
        let folder = derive_floor(bench(20.0, 42.0, 240.0), 12);
        assert!(matches!(folder, FloorPlan::FromBench { .. }));
        assert_eq!(folder.floor(), plan.floor());
    }

    #[test]
    fn floor_conservative_when_bench_missing_or_partial() {
        assert_eq!(derive_floor(None, 12), FloorPlan::Conservative { floor: 3 });
        // Any non-positive field = incomplete record (pre-v0.9.23 saves have lat 0.0).
        assert_eq!(derive_floor(bench(20.0, 42.0, 0.0), 12).floor(), 3);
        assert_eq!(derive_floor(bench(0.0, 42.0, 148.0), 12).floor(), 3);
        assert_eq!(derive_floor(bench(20.0, 0.0, 148.0), 12).floor(), 3);
        assert!(matches!(derive_floor(bench(20.0, 42.0, 0.0), 12), FloorPlan::Conservative { .. }));
        // The conservative constant still respects a tiny machine.
        assert_eq!(derive_floor(None, 2), FloorPlan::Conservative { floor: 2 });
    }

    /// v0.9.62 (B2.3): the typed accessor — schema gate, tier pick and provenance in ONE place.
    /// FALSIFIER (assert granularity): delete the `s.bench_ver != BENCH_SCHEMA_VER` early return →
    /// the `"a stale-schema record derives nothing"` assert reddens; swap the two tier arms → the
    /// `"the SUPER tier's latency"` assert reddens; drop `synthetic: s.bench_synthetic` (hardcode
    /// false) → the `"provenance rides with the numbers"` assert reddens and the floor's boot line
    /// would go back to claiming a folder measurement it never made.
    #[test]
    fn floor_bench_from_settings_owns_the_gate_the_tier_and_the_provenance() {
        let mut s = Settings::default();
        s.bench_ver = crate::BENCH_SCHEMA_VER;
        s.quality_super = false; // start on the SUB tier explicitly (the default ships Super)
        s.scrub_fps = 20.0;
        s.bench_sub_fps = 42;
        s.bench_super_fps = 21;
        s.bench_sub_lat_ms = 148.0;
        s.bench_super_lat_ms = 310.0;
        s.bench_synthetic = true;

        let sub = FloorBench::from_settings(&s).expect("a current-schema record derives");
        assert_eq!(sub.sustained_fps, 42.0);
        assert_eq!(sub.lat_ms, 148.0);
        assert_eq!(sub.applied_fps, 20.0);
        assert!(sub.synthetic, "provenance rides with the numbers");

        s.quality_super = true;
        let sup = FloorBench::from_settings(&s).unwrap();
        assert_eq!(sup.lat_ms, 310.0, "the SUPER tier's latency, because that is the applied tier");
        assert_eq!(sup.sustained_fps, 21.0);

        s.bench_ver = crate::BENCH_SCHEMA_VER - 1;
        assert!(
            FloorBench::from_settings(&s).is_none(),
            "a stale-schema record derives nothing"
        );
    }

    // ── ceiling derivation ───────────────────────────────────────────────────────────────

    #[test]
    fn ceiling_prefers_p_cores() {
        assert_eq!(derive_ceiling(Some(8), 12, 3), 8); // the M4 Pro: 8P+4E → ceiling 8
        assert_eq!(derive_ceiling(Some(10), 14, 3), 10); // unbinned M4 Pro: 10P+4E
    }

    #[test]
    fn ceiling_fallback_without_perflevel() {
        assert_eq!(derive_ceiling(None, 8, 3), 6); // Intel Mac: cores − 2
        assert_eq!(derive_ceiling(None, 4, 2), 2); // floor of the reserve rule
        assert_eq!(derive_ceiling(Some(0), 8, 3), 6); // insane sysctl → fallback
        assert_eq!(derive_ceiling(Some(64), 8, 3), 6); // p > cores is insane → fallback
    }

    #[test]
    fn ceiling_never_below_floor() {
        // The smoothness floor wins over P-count (the flagged corner: floor is a FLOOR).
        assert_eq!(derive_ceiling(Some(8), 12, 10), 10);
        assert_eq!(derive_ceiling(None, 4, 4), 4);
    }

    // ── the decision table, exhaustively ─────────────────────────────────────────────────

    fn sample(zone: crate::l2::PressureZone, delivered: f32, target: f32, q: usize) -> GovSample {
        // v0.9.60 (W2-3): `thumb_done: 0` — every row in this module delivers fast frames, so none
        // of them is an IDLE sample and the pre-existing table's tests keep their exact meaning.
        // The idle arm has its own rows below, which is where a zero-delivery sample belongs.
        // v0.9.61 (A3): `inflight: 0` for the same reason — these rows are about the pressure table.
        GovSample {
            delivered_fps: delivered,
            target_fps: target,
            queue_depth: q,
            zone,
            thumb_done: 0,
            inflight: 0,
            held: false,
        }
    }
    /// The idle sample: nothing queued, nothing in flight, nothing delivered, no thumbnail finished.
    /// v1.0 MERGE (F1-2): …and no gesture hold — a HELD window is suppressed, not idle, and has its
    /// own row below.
    fn idle_sample(zone: crate::l2::PressureZone) -> GovSample {
        GovSample {
            delivered_fps: 0.0,
            target_fps: 20.0,
            queue_depth: 0,
            zone,
            thumb_done: 0,
            inflight: 0,
            held: false,
        }
    }
    const P: GovParams = GovParams { floor: 3, ceiling: 8 };
    const STARVED: (f32, f32, usize) = (12.0, 20.0, 5);
    const SATISFIED: (f32, f32, usize) = (24.0, 20.0, 5);

    #[test]
    fn grow_needs_full_streak_of_starved_calm() {
        let mut st = GovState::new(3);
        let s = sample(Calm, STARVED.0, STARVED.1, STARVED.2);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Grow { from: 3, to: 4 });
        // The applied step resets the streak — the next grow needs a FULL fresh streak.
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Grow { from: 4, to: 5 });
    }

    #[test]
    fn a_satisfied_or_empty_queue_sample_resets_the_grow_streak() {
        let mut st = GovState::new(3);
        let starving = sample(Calm, STARVED.0, STARVED.1, STARVED.2);
        govern(&mut st, &P, &starving);
        govern(&mut st, &P, &starving);
        // Delivered ≥ target (settle) — streak dies.
        assert_eq!(govern(&mut st, &P, &sample(Calm, SATISFIED.0, SATISFIED.1, SATISFIED.2)), GovDecision::Hold);
        govern(&mut st, &P, &starving);
        govern(&mut st, &P, &starving);
        // Queue drained (idle) — slow delivery alone is NOT starvation.
        assert_eq!(govern(&mut st, &P, &sample(Calm, 5.0, 20.0, 0)), GovDecision::Hold);
        // Still needs the full streak after both resets.
        govern(&mut st, &P, &starving);
        govern(&mut st, &P, &starving);
        assert_eq!(govern(&mut st, &P, &starving), GovDecision::Grow { from: 3, to: 4 });
    }

    #[test]
    fn grow_holds_at_ceiling() {
        let mut st = GovState::new(8);
        let s = sample(Calm, STARVED.0, STARVED.1, STARVED.2);
        for _ in 0..10 {
            assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold);
        }
        assert_eq!(st.width, 8);
    }

    #[test]
    fn shrink_needs_its_streak_and_holds_at_floor() {
        let mut st = GovState::new(5);
        let low = sample(Low, SATISFIED.0, SATISFIED.1, SATISFIED.2);
        assert_eq!(govern(&mut st, &P, &low), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &low), GovDecision::Shrink { from: 5, to: 4, cause: ShrinkCause::Pressure });
        assert_eq!(govern(&mut st, &P, &low), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &low), GovDecision::Shrink { from: 4, to: 3, cause: ShrinkCause::Pressure });
        // At the floor: pressure may persist forever, the width never goes below.
        for _ in 0..10 {
            assert_eq!(govern(&mut st, &P, &low), GovDecision::Hold);
        }
        assert_eq!(st.width, 3);
    }

    #[test]
    fn low_zone_shrinks_even_while_starved() {
        // Pressure BEATS starvation — the exact M3 failure this round closes (never grow into a
        // memory squeeze; the smoothness floor is the only lower bound).
        let mut st = GovState::new(6);
        let s = sample(Low, STARVED.0, STARVED.1, STARVED.2);
        govern(&mut st, &P, &s);
        assert_eq!(govern(&mut st, &P, &s), GovDecision::Shrink { from: 6, to: 5, cause: ShrinkCause::Pressure });
    }

    #[test]
    fn dead_band_resets_both_streaks() {
        let mut st = GovState::new(5);
        let starving = sample(Calm, STARVED.0, STARVED.1, STARVED.2);
        let low = sample(Low, SATISFIED.0, SATISFIED.1, SATISFIED.2);
        let dead = sample(Dead, SATISFIED.0, SATISFIED.1, SATISFIED.2);
        // 2 grow credits, then Dead: gone.
        govern(&mut st, &P, &starving);
        govern(&mut st, &P, &starving);
        assert_eq!(govern(&mut st, &P, &dead), GovDecision::Hold);
        govern(&mut st, &P, &starving);
        govern(&mut st, &P, &starving);
        assert_eq!(govern(&mut st, &P, &starving), GovDecision::Grow { from: 5, to: 6 });
        // 1 shrink credit, then Dead: gone.
        govern(&mut st, &P, &low);
        assert_eq!(govern(&mut st, &P, &dead), GovDecision::Hold);
        govern(&mut st, &P, &low);
        assert_eq!(govern(&mut st, &P, &low), GovDecision::Shrink { from: 6, to: 5, cause: ShrinkCause::Pressure });
    }

    #[test]
    fn zone_flips_never_oscillate_width() {
        // Alternate Low/Calm(starved) forever: neither streak can complete — width is a flat line.
        // (In reality the Dead band separates the zones by 1.5 GiB, so even this adversarial
        // sequence is unreachable; the state machine holds regardless.)
        let mut st = GovState::new(5);
        for k in 0..40 {
            let s = if k % 2 == 0 {
                sample(Low, STARVED.0, STARVED.1, STARVED.2)
            } else {
                sample(Calm, STARVED.0, STARVED.1, STARVED.2)
            };
            assert_eq!(govern(&mut st, &P, &s), GovDecision::Hold, "sample {k}");
        }
        assert_eq!(st.width, 5);
    }

    #[test]
    fn full_journey_floor_to_ceiling_and_back() {
        // A realistic session: hard scrub grows floor→ceiling; a memory squeeze walks it back.
        let mut st = GovState::new(3);
        let starving = sample(Calm, 15.0, 20.0, 8);
        let mut grows = 0;
        for _ in 0..(POOL_GROW_STREAK as usize * 10) {
            if matches!(govern(&mut st, &P, &starving), GovDecision::Grow { .. }) {
                grows += 1;
            }
        }
        assert_eq!(st.width, 8, "reaches the ceiling");
        assert_eq!(grows, 5, "3→8 in exactly five +1 steps");
        let low = sample(Low, 30.0, 20.0, 0);
        for _ in 0..(POOL_SHRINK_STREAK as usize * 10) {
            govern(&mut st, &P, &low);
        }
        assert_eq!(st.width, 3, "pressure walks it back to the floor, never below");
    }

    // ── v0.9.60 (W2-3): the IDLE-SHRINK arm ──────────────────────────────────────────────

    /// The ratchet is no longer one-way: an idle pool returns to the floor in ONE step.
    /// FALSIFIER: delete the idle arm from `govern` (or raise `POOL_IDLE_SHRINK_SAMPLES` above the
    /// loop count) and the width stays at 8 — the exact round-4 log this arm exists to answer.
    #[test]
    fn an_idle_pool_falls_back_to_the_floor_in_one_step() {
        let mut st = GovState::new(8);
        let idle = idle_sample(Calm);
        for k in 1..POOL_IDLE_SHRINK_SAMPLES {
            assert_eq!(govern(&mut st, &P, &idle), GovDecision::Hold, "sample {k} is too early");
        }
        assert_eq!(govern(&mut st, &P, &idle), GovDecision::Shrink { from: 8, to: 3, cause: ShrinkCause::Idle });
        assert_eq!(st.width, 3);
        // At the floor an idle pool holds forever — there is nothing below the designed width.
        for _ in 0..(POOL_IDLE_SHRINK_SAMPLES * 3) {
            assert_eq!(govern(&mut st, &P, &idle), GovDecision::Hold);
        }
        assert_eq!(st.width, 3);
    }

    /// ONE working sample resets the clock — a browse pause shorter than the window keeps the
    /// width the governor grew for that browse.
    /// FALSIFIER: drop the `st.idle_streak = 0` in `govern`'s non-idle arm and the interleaved
    /// sequence below shrinks anyway.
    #[test]
    fn any_work_at_all_resets_the_idle_clock() {
        let mut st = GovState::new(8);
        let idle = idle_sample(Calm);
        for _ in 0..4 {
            for _ in 0..(POOL_IDLE_SHRINK_SAMPLES - 1) {
                assert_eq!(govern(&mut st, &P, &idle), GovDecision::Hold);
            }
            // one satisfied window (frames delivered, queue drained) — not idle
            assert_eq!(govern(&mut st, &P, &sample(Calm, 24.0, 20.0, 0)), GovDecision::Hold);
        }
        assert_eq!(st.width, 8, "the width the browse earned survives sub-window pauses");
    }

    /// A THUMBNAIL-only window is work: a filmstrip scroll delivers no fast frames and drains its
    /// queue, and must not be mistaken for an idle pool.
    /// FALSIFIER: drop the `s.thumb_done == 0` term from `pool_idle` and this shrinks to the floor
    /// mid-scroll.
    #[test]
    fn thumbnail_only_windows_are_not_idle() {
        let mut st = GovState::new(8);
        let scrolling = GovSample {
            delivered_fps: 0.0,
            target_fps: 20.0,
            queue_depth: 0,
            zone: Calm,
            thumb_done: 12,
            inflight: 0,
            held: false,
        };
        for _ in 0..(POOL_IDLE_SHRINK_SAMPLES * 3) {
            assert_eq!(govern(&mut st, &P, &scrolling), GovDecision::Hold);
        }
        assert_eq!(st.width, 8);
    }

    /// v1.0 MERGE (F1-2): A HELD POOL IS NOT AN IDLE POOL. Trunk's fast-tier dispatch hold drops
    /// every speculative want for the length of a gesture, so a continuous zoom-drag on an
    /// already-cached frame produces a sample that is idle by all four of the old terms and is in
    /// fact the app deliberately not asking. Shrinking here is the merge-created regression F1-2
    /// filed: the width the gesture's release needs would be gone by the time it arrives.
    /// FALSIFIER (L28): drop the `!s.held` term from `pool_idle` and the width falls to the floor on
    /// the tenth sample, so the `st.width == 8` assert below reddens.
    #[test]
    fn a_held_pool_is_not_an_idle_pool() {
        let mut st = GovState::new(8);
        let held = GovSample {
            delivered_fps: 0.0,
            target_fps: 20.0,
            queue_depth: 0,
            zone: Calm,
            thumb_done: 0,
            inflight: 0,
            held: true,
        };
        for _ in 0..(POOL_IDLE_SHRINK_SAMPLES * 3) {
            assert_eq!(govern(&mut st, &P, &held), GovDecision::Hold);
        }
        assert_eq!(st.width, 8, "a pool the gate is holding keeps the width the release will need");
        // …and when the gesture ends and the pool really is quiet, the clock runs its course.
        let idle = idle_sample(Calm);
        for k in 1..POOL_IDLE_SHRINK_SAMPLES {
            assert_eq!(govern(&mut st, &P, &idle), GovDecision::Hold, "sample {k} is too early");
        }
        assert!(matches!(govern(&mut st, &P, &idle), GovDecision::Shrink { .. }));
    }

    /// v0.9.61 (A3 / J15): A DECODE IN FLIGHT IS NOT AN IDLE POOL. The queue is empty because the
    /// job was POPPED, and the window delivered nothing because the decode has not finished — the
    /// exact sample a slow 48 MP RAW produces, once a second, for as long as it takes. Shrinking
    /// under it would take workers away from the work that is running.
    /// FALSIFIER: drop the `s.inflight == 0` term from `pool_idle` and the width falls to the floor
    /// on the tenth sample, so the `st.width == 8` assert below reddens.
    #[test]
    fn a_decode_in_flight_is_not_an_idle_pool() {
        let mut st = GovState::new(8);
        let long_decode = GovSample {
            delivered_fps: 0.0,
            target_fps: 20.0,
            queue_depth: 0,
            zone: Calm,
            thumb_done: 0,
            inflight: 1,
            held: false,
        };
        for _ in 0..(POOL_IDLE_SHRINK_SAMPLES * 3) {
            assert_eq!(govern(&mut st, &P, &long_decode), GovDecision::Hold);
        }
        assert_eq!(st.width, 8, "a pool with work in flight keeps the width that work is using");
        // …and the moment it really does go quiet, the clock starts from zero and runs its course.
        let idle = idle_sample(Calm);
        for k in 1..POOL_IDLE_SHRINK_SAMPLES {
            assert_eq!(govern(&mut st, &P, &idle), GovDecision::Hold, "sample {k} is too early");
        }
        assert_eq!(
            govern(&mut st, &P, &idle),
            GovDecision::Shrink { from: 8, to: 3, cause: ShrinkCause::Idle }
        );
    }

    /// v0.9.61 (A3 / J19): THE CAUSE IS THE ONE THE STATE MACHINE TOOK. v0.9.60's log re-derived it
    /// from `to == floor && zone != Low`, a DIFFERENT predicate — so a pressure shrink whose single
    /// step lands on the floor reported "idle", and an idle shrink taken on a Low sample reported
    /// "RAM pressure". Both directions are pinned here.
    /// FALSIFIER: return `ShrinkCause::Pressure` from the idle arm of `govern` (or `Idle` from the
    /// pressure arm of `govern_pressure`) and one of the two asserts below reddens.
    #[test]
    fn a_shrink_names_the_cause_it_was_actually_taken_for() {
        // 1) A PRESSURE shrink that lands exactly on the floor still says Pressure — the width-based
        //    inference would have called this one "idle".
        const TIGHT: GovParams = GovParams { floor: 4, ceiling: 8 };
        let mut st = GovState::new(5);
        let low = sample(Low, 24.0, 20.0, 5);
        assert_eq!(govern(&mut st, &TIGHT, &low), GovDecision::Hold);
        assert_eq!(
            govern(&mut st, &TIGHT, &low),
            GovDecision::Shrink { from: 5, to: 4, cause: ShrinkCause::Pressure },
            "landing on the floor does not make a pressure shrink an idle one"
        );
        // 2) An IDLE shrink that happens to land on the FIRST Low sample still says Idle. Nine idle
        //    Calm samples bank the clock; the tenth arrives in the Low zone, where the pressure
        //    table's own streak (2) is not yet complete, so it Holds and the idle arm fires.
        let mut st = GovState::new(8);
        let idle_calm = idle_sample(Calm);
        for _ in 0..(POOL_IDLE_SHRINK_SAMPLES - 1) {
            assert_eq!(govern(&mut st, &P, &idle_calm), GovDecision::Hold);
        }
        assert_eq!(
            govern(&mut st, &P, &idle_sample(Low)),
            GovDecision::Shrink { from: 8, to: 3, cause: ShrinkCause::Idle },
            "the FIRST Low sample cannot complete the pressure streak — this shrink is the idle clock's"
        );
    }

    /// Pressure still outranks the idle clock: a Low-zone sample takes its own −1 step on schedule
    /// even while the pool is idle, and the idle streak keeps running underneath it.
    /// FALSIFIER (L28, at ASSERT granularity — v0.9.61 / J5 corrected this; the v0.9.59 wording
    /// named the first assert, which does not move): run the idle arm BEFORE `govern_pressure` and
    /// return its answer (swap the two halves of `govern`) → the SECOND assert reddens,
    /// `assert_eq!(govern(&mut st, &P, &idle_low), GovDecision::Shrink { from: 8, to: 7, cause:
    /// Pressure })` returning `Hold`, because the idle clock has only banked two of its ten samples
    /// and the pressure shrink it swallowed never reaches the caller. The FIRST assert expects
    /// `Hold` and gets `Hold` under both orderings — naming it left the ordering effectively
    /// unfalsified.
    #[test]
    fn pressure_outranks_the_idle_clock() {
        let mut st = GovState::new(8);
        let idle_low = idle_sample(Low);
        assert_eq!(govern(&mut st, &P, &idle_low), GovDecision::Hold);
        assert_eq!(govern(&mut st, &P, &idle_low), GovDecision::Shrink { from: 8, to: 7, cause: ShrinkCause::Pressure });
    }

    // ── ElasticGate: the width protocol ──────────────────────────────────────────────────

    #[test]
    fn gate_resize_semantics() {
        let g = ElasticGate::new(3, 8);
        assert_eq!(g.target(), 3);
        assert_eq!(g.alive_count(), 3);
        // Grow: exactly the dead slots below the new target, marked alive atomically.
        assert_eq!(g.resize(5), vec![3, 4]);
        assert_eq!(g.alive_count(), 5);
        // Idempotent: a repeat resize spawns nothing (the double-spawn guard).
        assert_eq!(g.resize(5), Vec::<usize>::new());
        // Shrink: nothing to spawn; workers 3+ exit at their next check.
        assert_eq!(g.resize(3), Vec::<usize>::new());
        assert!(!g.worker_should_exit(0), "slot below target stays");
        assert!(g.worker_should_exit(4), "slot at/above target exits (slot marked dead)");
        assert!(g.worker_should_exit(3));
        assert_eq!(g.alive_count(), 3);
        // Regrow AFTER the exits: the dead slots come back as spawn orders.
        assert_eq!(g.resize(5), vec![3, 4]);
        // Capacity clamp: can never resize past the ceiling allocation.
        assert_eq!(g.resize(64), vec![5, 6, 7]);
        assert_eq!(g.target(), 8);
    }

    #[test]
    fn gate_regrow_before_exit_keeps_the_worker() {
        // The race the shared lock kills: target drops (worker hasn't checked yet), then rises
        // again. The worker's next check sees the RAISED target and stays; resize spawned nothing
        // for its still-alive slot — exactly one worker per slot, always.
        let g = ElasticGate::new(4, 8);
        g.resize(3); // slot 3 condemned…
        assert_eq!(g.resize(4), Vec::<usize>::new(), "…but never exited: alive → no respawn");
        assert!(!g.worker_should_exit(3), "the raised target reprieves it");
        assert_eq!(g.alive_count(), 4);
    }

    /// The phantom-slot fix (v0.9.23 addendum): `resize` marks a grown slot alive BEFORE the
    /// fallible spawn; when the spawn FAILS the governor calls `mark_dead` so the next `resize`
    /// re-offers the slot instead of the double-spawn guard silently spawning nothing.
    #[test]
    fn mark_dead_reopens_slot_for_respawn() {
        let g = ElasticGate::new(3, 8);
        assert_eq!(g.resize(4), vec![3], "grow offers the new slot (marked alive pre-spawn)");
        assert_eq!(g.alive_count(), 4);
        // simulate the OS refusing the worker thread for slot 3:
        g.mark_dead(3);
        assert_eq!(g.alive_count(), 3, "mark_dead reopened the phantom slot");
        assert_eq!(g.target(), 4, "target is untouched — the governor rolls its own width back");
        // the crux: WITHOUT mark_dead this resize would return [] (guard sees it alive); WITH it,
        // the reopened slot is re-offered so the next grow can respawn a real worker.
        assert_eq!(g.resize(4), vec![3], "a subsequent resize re-offers the reopened slot");
        assert_eq!(g.alive_count(), 4);
        // out-of-range slot: a no-op, never panics.
        g.mark_dead(999);
        assert_eq!(g.alive_count(), 4);
    }

    /// v0.9.61 (A3 / J20): THE PARK RE-CHECK IS A QUESTION, NOT AN ACT. `worker_should_exit` marks
    /// the slot dead under the gate lock — correct at the one site that then returns, wrong anywhere
    /// it is asked as a predicate. The park's pre-sleep re-check asked it, so a worker that then
    /// parked anyway had already erased itself from the alive map and the next grow spawned a SECOND
    /// worker onto its slot. `slot_condemned` answers the same question and touches nothing.
    /// FALSIFIER: point `slot_condemned` at `worker_should_exit` (`self.worker_should_exit(slot)`)
    /// and the `alive_count` assert after the condemned read reddens — 4 becomes 3 — and the
    /// double-spawn assert at the end reddens too, because the resize re-offers a live slot.
    #[test]
    fn the_park_recheck_answers_without_mutating_the_gate() {
        let g = ElasticGate::new(4, 8);
        g.resize(2); // slots 2 and 3 are condemned, but nobody has exited yet
        assert!(g.slot_condemned(3), "the hint sees the new target");
        assert!(g.slot_condemned(2));
        assert!(!g.slot_condemned(1), "a slot below the target is not condemned");
        assert_eq!(g.alive_count(), 4, "asking must not kill a worker that has not exited");
        // The authoritative call is the one that acts — and it is the ONLY one that acts.
        assert!(g.worker_should_exit(3));
        assert_eq!(g.alive_count(), 3, "…and that one does mark the slot dead");
        // The invariant the mutating re-check broke: a regrow must not offer a slot whose worker is
        // still alive. Slot 2 never exited, so 3 (which did) is the only spawn order.
        assert_eq!(g.resize(4), vec![3], "only the slot that really exited is respawned");
        assert_eq!(g.alive_count(), 4);
        // The hint tracks every resize, including a grow.
        assert!(!g.slot_condemned(3));
        g.resize(8);
        assert!(!g.slot_condemned(7));
    }

    /// The no-task-loss proof (spec item I): a real mini-pool over a Pump-shaped queue processes
    /// EVERY job exactly once across a shrink mid-drain and a regrow — workers exit only between
    /// jobs, the queue is never touched by resizing, survivors drain the backlog.
    ///
    /// v0.9.61 (A3 / J9): THE WORKER BODY HERE IS THE SHIPPED ONE. It used to model the v0.9.23
    /// worker — a 5 ms `wait_timeout` poll — which is not what `elastic_pool_worker` has done since
    /// v0.9.60: the park is UNBOUNDED, and it is safe only because of a protocol (re-read the work
    /// sequence and the gate hint UNDER the pump mutex before sleeping; the producer bumps the
    /// sequence, takes and releases that mutex, then broadcasts). A proof test that polls cannot
    /// fail on a broken protocol — a lost wakeup would simply be papered over by the next timeout,
    /// exactly as it was in production before v0.9.60 — so this now parks unbounded, re-checks under
    /// the mutex, and would DEADLOCK (caught by the bounded drain assert) if the protocol regressed.
    #[test]
    fn elastic_gate_no_task_loss_under_resize() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        use std::sync::{Arc, Condvar, Mutex};
        const FAST: usize = 400; // pre-queued on the PUMP (the fast tier)
        const THUMBS: usize = 120; // delivered later on a SEPARATE queue (the thumb channel)
        const JOBS: usize = FAST + THUMBS;
        let gate = Arc::new(ElasticGate::new(6, 8));
        let pump: Arc<(Mutex<VecDeque<usize>>, Condvar)> =
            Arc::new((Mutex::new((0..FAST).collect()), Condvar::new()));
        // The SECOND work source, with its OWN lock — the shape that makes the sequence counter
        // load-bearing. A worker parking on the pump condvar cannot see this queue's contents in the
        // guard it holds, so only the counter can tell it "work arrived while you were deciding".
        let thumbs: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new(VecDeque::new()));
        let seq = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(vec![0u8; JOBS]));
        let spawn_worker = |slot: usize| {
            let gate = gate.clone();
            let pump = pump.clone();
            let thumbs = thumbs.clone();
            let seq = seq.clone();
            let done = done.clone();
            let seen = seen.clone();
            std::thread::spawn(move || loop {
                // THE SHIPPED SHAPE, step for step (main.rs `elastic_pool_worker`):
                // 1. the ONE mutating gate check, at the top, immediately followed by a return;
                // 2. read the work SEQUENCE before looking at either queue;
                // 3. try both queues (fast first, then the second source);
                // 4. nothing → take the pump mutex, RE-READ the sequence and the (non-mutating)
                //    gate hint under it, and only then park UNBOUNDED.
                if gate.worker_should_exit(slot) {
                    pump.1.notify_all(); // hand the wake on (the shipped worker's exit broadcast)
                    return; // between jobs only — never holding one
                }
                let seq0 = seq.load(Ordering::SeqCst);
                let job = {
                    let (m, _cv) = &*pump;
                    let mut q = m.lock().unwrap();
                    q.pop_front()
                }
                .or_else(|| thumbs.lock().unwrap().pop_front());
                match job {
                    Some(j) => {
                        std::thread::sleep(std::time::Duration::from_micros(200)); // "decode"
                        seen.lock().unwrap()[j] += 1;
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        let (m, cv) = &*pump;
                        let guard = m.lock().unwrap();
                        if seq.load(Ordering::SeqCst) != seq0
                            || !guard.is_empty()
                            || gate.slot_condemned(slot)
                        {
                            continue; // work (or a condemnation) arrived while we decided to sleep
                        }
                        let _unparked = cv.wait(guard).unwrap();
                    }
                }
            })
        };
        let mut handles: Vec<_> = (0..6).map(spawn_worker).collect();
        // The RELAY, with the shipped discipline: enqueue, bump the sequence, take AND RELEASE the
        // pump mutex, then broadcast. The acquisition is the ordering half of the proof — a worker
        // either sees the bump at its re-read (it holds this mutex there) or is already inside
        // `wait` and the broadcast reaches it. There is no third case, and with an unbounded park
        // there is no timeout to paper over one.
        let relay = {
            let pump = pump.clone();
            let thumbs = thumbs.clone();
            let seq = seq.clone();
            std::thread::spawn(move || {
                for batch in 0..(THUMBS / 8) {
                    {
                        let mut t = thumbs.lock().unwrap();
                        for k in 0..8 {
                            t.push_back(FAST + batch * 8 + k);
                        }
                    }
                    seq.fetch_add(1, Ordering::SeqCst);
                    drop(pump.0.lock().unwrap());
                    pump.1.notify_all();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })
        };
        // Mid-drain: shrink 6→2 (condemns 4 workers holding/near jobs), then grow 2→4.
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(gate.resize(2).is_empty());
        pump.1.notify_all(); // the governor's post-shrink wake, so parked workers re-check
        std::thread::sleep(std::time::Duration::from_millis(10));
        for slot in gate.resize(4) {
            handles.push(spawn_worker(slot));
        }
        // Wait for the drain (bounded). A regressed park protocol DEADLOCKS here rather than being
        // rescued by a timeout — which is the whole reason this body had to stop polling.
        let t0 = std::time::Instant::now();
        while done.load(Ordering::Relaxed) < JOBS {
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(20),
                "drain stalled — jobs lost, or a wakeup was missed with nothing to paper over it"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = relay.join();
        // Exactly-once: every job seen exactly one time.
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|&n| n == 1), "every job processed exactly once");
        // Convergence: after the drain, workers above the target exit on their next check.
        let t0 = std::time::Instant::now();
        while gate.alive_count() > gate.target() {
            pump.1.notify_all();
            assert!(t0.elapsed() < std::time::Duration::from_secs(10), "workers failed to converge");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(gate.alive_count(), 4);
        // Hygiene: retire the pool so no parked thread outlives the test.
        assert!(gate.resize(0).is_empty());
        pump.1.notify_all();
        for h in handles {
            let _ = h.join();
        }
        assert_eq!(gate.alive_count(), 0);
    }
}
