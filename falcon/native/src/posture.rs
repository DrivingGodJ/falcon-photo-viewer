//! v0.9.9 (P6): the TEMPORARY dev "posture benchmark" — a measurement instrument for the upcoming
//! battery/posture design. It grafts onto the §43 benchmark infrastructure (same synthetic frames,
//! same fast-tier decode the live pool uses) but is otherwise independent: LOG-ONLY output
//! (`posture-bench: …` lines in falcon.log) — it NEVER routes results through `drain_bench` / the §43
//! UI fields. It exists to turn the E-core-throughput / pool-width-scaling / Apple-media-engine
//! numbers (today estimates) into real measurements a non-dev Mac tester can produce.
//!
//! This module is the PURE core (plan builder + matrix runner + QoS binding + result types), unit-
//! tested headlessly. The orchestration (header line, synthetic frames, the Image I/O arm, the
//! per-line logging, the done line) lives in `main.rs` at the callback site, where `synth_bench_dir`,
//! `ImageIODecoder`, and the runtime atomics live. Removed once the posture design lands.

use falcon_decode::{fast_frame_rgba, Shot};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The scheduling class a CPU-matrix cell runs its workers under. `Utility` is measured only on
/// macOS (E-core-affine QoS band); on Windows the plan builder never emits a `Utility` cell, so
/// this variant is inert there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PostureQos {
    /// The default scheduling class — the app's real decode-pool posture.
    Default,
    /// `QOS_CLASS_UTILITY` (macOS only) — nudges the worker onto the efficiency cores so the bench
    /// can measure E-core throughput vs the default P-core-preferred placement.
    Utility,
}

impl PostureQos {
    /// The log-line token (`qos=default` / `qos=utility`).
    pub(crate) fn tag(self) -> &'static str {
        match self {
            PostureQos::Default => "default",
            PostureQos::Utility => "utility",
        }
    }
}

/// One cell of the CPU throughput matrix: `width` decode-pool workers under scheduling class `qos`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PostureCell {
    /// Worker-thread count for this cell.
    pub(crate) width: usize,
    /// The scheduling class the workers run under.
    pub(crate) qos: PostureQos,
}

/// The measured result of one cell. `err` is `Some` when the cell produced no successful decode (the
/// instrument logs a `FAILED` line for it and continues — it never aborts the run).
#[derive(Clone, Debug)]
pub(crate) struct PostureResult {
    pub(crate) width: usize,
    pub(crate) qos: PostureQos,
    /// Throughput = successfully-decoded frames ÷ wall-clock seconds.
    pub(crate) fps: f64,
    /// Mean per-frame decode time (ms) across every worker's successful decodes.
    pub(crate) avg_ms: f64,
    /// Median per-frame decode time (ms).
    pub(crate) med_ms: f64,
    /// Successfully-decoded frame count (equals the planned frame count when every decode succeeds).
    pub(crate) frames: usize,
    /// `Some(why)` when the cell decoded nothing — the orchestration logs a `FAILED` line.
    pub(crate) err: Option<String>,
}

/// Mean + median of a sample set (ms), sorting `samples` in place for the median. `(0.0, 0.0)` for an
/// empty set. Shared by the CPU matrix and the Image I/O latency arm.
pub(crate) fn avg_med(samples: &mut [f64]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let sum: f64 = samples.iter().sum();
    let avg = sum / samples.len() as f64;
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = samples.len() / 2;
    let med = if samples.len() % 2 == 0 {
        (samples[mid - 1] + samples[mid]) / 2.0
    } else {
        samples[mid]
    };
    (avg, med)
}

/// Build the CPU throughput matrix plan. Widths {2, 3, 4, 6, 8, `full_pool_width`} — the last deduped
/// if it collides with a fixed width (the common case on an ≤10-logical-core machine, where the pool
/// caps at 8). `include_utility` (macOS only — the caller passes `cfg!(target_os = "macos")`) adds a
/// second `Utility`-QoS cell per width; on Windows the matrix is `Default`-only. Pure — unit-tested.
pub(crate) fn build_posture_plan(full_pool_width: usize, include_utility: bool) -> Vec<PostureCell> {
    let base = [2usize, 3, 4, 6, 8];
    let mut widths: Vec<usize> = base.to_vec();
    if !base.contains(&full_pool_width) {
        widths.push(full_pool_width);
    }
    let mut plan = Vec::with_capacity(widths.len() * if include_utility { 2 } else { 1 });
    for &w in &widths {
        plan.push(PostureCell { width: w, qos: PostureQos::Default });
        if include_utility {
            plan.push(PostureCell { width: w, qos: PostureQos::Utility });
        }
    }
    plan
}

/// macOS: pin the CURRENT thread to the `QOS_CLASS_UTILITY` band so its work is scheduled toward the
/// efficiency cores. Direct FFI (the libc crate carries no `pthread_set_qos_class_self_np` binding as
/// of 0.2.x — the same zero-new-deps direct-FFI route the v0.9.8 CoreGraphics ICC arm took).
/// `QOS_CLASS_UTILITY` = `0x11` per `<sys/qos.h>`; a relative priority of 0 = the band's default.
#[cfg(target_os = "macos")]
fn apply_utility_qos() {
    // `qos_class_t` is an unsigned enum (c_uint); the second arg is a signed relative priority.
    extern "C" {
        fn pthread_set_qos_class_self_np(
            qos_class: libc::c_uint,
            relative_priority: libc::c_int,
        ) -> libc::c_int;
    }
    const QOS_CLASS_UTILITY: libc::c_uint = 0x11;
    // Best-effort: a non-zero return (never observed for a valid band) just leaves the thread at its
    // default QoS — the cell still measures, it simply isn't E-core-nudged. Never a panic.
    unsafe {
        let _ = pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
    }
}

/// Run ONE matrix cell: spawn `width` scoped workers that pull the next frame index off a shared
/// atomic (the exact work-steal pool pattern the app's decode pool + the §43 bench use) and decode it
/// through the SAME fast-tier entry the live pool runs (`fast_frame_rgba(shot, stop, true)`). Each
/// worker records its successful decodes' per-frame ms; the cell's fps = total decoded ÷ wall-clock.
/// A worker panic is swallowed (`join().unwrap_or_default()` → no durations) so a cell can degrade to
/// `err` but never aborts the run.
fn run_cell(width: usize, qos: PostureQos, shots: &[Shot], stop: u32, frames: usize) -> PostureResult {
    let n = shots.len().max(1);
    let counter = AtomicUsize::new(0);
    let wall_start = Instant::now();
    let per: Vec<Vec<f64>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(width.max(1));
        for _ in 0..width.max(1) {
            let counter = &counter;
            let shots = shots;
            handles.push(s.spawn(move || {
                // macOS Utility cell: nudge this worker to the E-cores before it decodes anything.
                #[cfg(target_os = "macos")]
                if matches!(qos, PostureQos::Utility) {
                    apply_utility_qos();
                }
                let mut durs: Vec<f64> = Vec::new();
                loop {
                    let j = counter.fetch_add(1, Ordering::Relaxed);
                    if j >= frames {
                        break;
                    }
                    if let Some(shot) = shots.get(j % n) {
                        let t = Instant::now();
                        if fast_frame_rgba(shot, stop, true).is_ok() {
                            durs.push(t.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
                durs
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let wall = wall_start.elapsed().as_secs_f64().max(0.001);
    let mut all: Vec<f64> = per.into_iter().flatten().collect();
    let decoded = all.len();
    if decoded == 0 {
        return PostureResult {
            width,
            qos,
            fps: 0.0,
            avg_ms: 0.0,
            med_ms: 0.0,
            frames: 0,
            err: Some("no frames decoded".to_string()),
        };
    }
    let (avg, med) = avg_med(&mut all);
    PostureResult { width, qos, fps: decoded as f64 / wall, avg_ms: avg, med_ms: med, frames: decoded, err: None }
}

/// Execute a matrix PLAN over `shots`, decoding each cell at `stop` (the sub-tier DCT stop). Per cell,
/// the frame count is `frames_override` when set, else `max(48, 6×width)` (the §43 pool-scaled rule,
/// so wide pools get enough frames for clean work-stealing). `on_cell` is invoked as EACH cell
/// completes (the orchestration logs a line there — progress visibility). Returns every cell's result.
/// Pure + UI-independent, so a headless unit test drives it with a tiny plan.
pub(crate) fn run_posture_matrix(
    plan: &[PostureCell],
    shots: &[Shot],
    stop: u32,
    frames_override: Option<usize>,
    mut on_cell: impl FnMut(&PostureResult),
) -> Vec<PostureResult> {
    let mut out = Vec::with_capacity(plan.len());
    for cell in plan {
        let frames = frames_override.unwrap_or((crate::BENCH_FRAMES_PER_WORKER * cell.width).max(48));
        let r = run_cell(cell.width, cell.qos, shots, stop, frames);
        on_cell(&r);
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan builder's shape: Windows is Default-only over {2,3,4,6,8,pool}; the pool width dedups
    /// when it collides with a fixed width; macOS doubles each width with a Utility cell.
    #[test]
    fn plan_builder_shape_and_dedup() {
        // Windows (include_utility = false): the full width list, Default-only, no Utility cell.
        let plan = build_posture_plan(18, false);
        assert_eq!(plan.iter().map(|c| c.width).collect::<Vec<_>>(), vec![2, 3, 4, 6, 8, 18]);
        assert!(plan.iter().all(|c| matches!(c.qos, PostureQos::Default)), "no Utility cells on Windows");

        // full_pool_width == 8 collides with the fixed list → deduped (no trailing duplicate 8).
        let plan8 = build_posture_plan(8, false);
        assert_eq!(plan8.iter().map(|c| c.width).collect::<Vec<_>>(), vec![2, 3, 4, 6, 8]);

        // A narrow pool (4-core box → pool 4) also dedups against the fixed list.
        let plan4 = build_posture_plan(4, false);
        assert_eq!(plan4.iter().map(|c| c.width).collect::<Vec<_>>(), vec![2, 3, 4, 6, 8]);

        // macOS (include_utility = true): each width gets a Default + a Utility cell.
        let mac = build_posture_plan(8, true);
        assert_eq!(mac.len(), 10, "5 widths × 2 qos");
        assert_eq!(mac.iter().filter(|c| matches!(c.qos, PostureQos::Utility)).count(), 5);
        assert_eq!(mac.iter().filter(|c| matches!(c.qos, PostureQos::Default)).count(), 5);
    }

    /// avg_med: mean + median, even and odd sample counts, empty set.
    #[test]
    fn avg_med_math() {
        assert_eq!(avg_med(&mut []), (0.0, 0.0));
        let (a, m) = avg_med(&mut [5.0]);
        assert_eq!((a, m), (5.0, 5.0));
        let (a, m) = avg_med(&mut [4.0, 2.0, 6.0]); // odd → middle after sort (2,4,6)
        assert_eq!((a, m), (4.0, 4.0));
        let (a, m) = avg_med(&mut [1.0, 3.0, 2.0, 4.0]); // even → mean of the two middles (2,3)
        assert_eq!((a, m), (2.5, 2.5));
    }

    /// Drop-guard temp dir for the matrix test — deletes its frames even if an assertion panics.
    struct TmpFrames(std::path::PathBuf);
    impl Drop for TmpFrames {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Encode 4 tiny JPEGs into a PROCESS-UNIQUE temp dir and scan them into Shots. We do NOT reuse
    /// `synth_bench_dir` here: it keys its temp dir on the PID ALONE, so two of its callers running
    /// concurrently (cargo runs the bin's tests in parallel — this test + the §43 `synth_bench_dir`
    /// test) would race the same directory and truncate each other's frames. A per-call unique dir
    /// keeps this test hermetic while leaving `synth_bench_dir` (and the §43 test) untouched.
    fn tiny_jpeg_shots(count: usize) -> (TmpFrames, Vec<Shot>) {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let (w, h) = (256u32, 170u32); // small 3:2-ish frame → fast decode
        let mut rgb = vec![0u8; (w as usize) * (h as usize) * 3];
        for (i, px) in rgb.iter_mut().enumerate() {
            *px = ((i * 37) & 0xff) as u8; // deterministic, non-flat content so the JPEG carries real AC
        }
        let bytes = falcon_decode::encode_jpeg_rgb(&rgb, w, h, 90).expect("encode tiny jpeg");
        let dir = std::env::temp_dir().join(format!("falcon_posture_test_{}_{}", std::process::id(), uniq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let guard = TmpFrames(dir.clone());
        for k in 0..count {
            std::fs::write(dir.join(format!("f{k:02}.jpg")), &bytes).expect("write frame");
        }
        let shots = falcon_decode::scan_folder(&dir).unwrap_or_default();
        (guard, shots)
    }

    /// The matrix runner, headless: a 2-cell Default plan (the Windows matrix shape) over 4 tiny
    /// synthetic JPEGs, frames-per-cell overridden to 4. Every cell must produce a valid, positive
    /// measurement over exactly 4 frames.
    #[test]
    fn matrix_runner_measures_every_cell() {
        let (_frames, shots) = tiny_jpeg_shots(4);
        assert!(shots.len() >= 4, "the generator produced at least 4 scannable JPEGs");

        // Two Default cells (widths 2 and 4) — unambiguously a 2-cell plan, the Windows matrix shape.
        let plan = vec![
            PostureCell { width: 2, qos: PostureQos::Default },
            PostureCell { width: 4, qos: PostureQos::Default },
        ];
        let mut logged = 0usize;
        let results = run_posture_matrix(&plan, &shots, 512, Some(4), |_| logged += 1);

        assert_eq!(results.len(), plan.len(), "one result per planned cell");
        assert_eq!(logged, plan.len(), "on_cell fired once per cell (progress visibility)");
        for r in &results {
            assert!(r.err.is_none(), "cell {r:?} decoded frames");
            assert!(r.fps > 0.0, "fps > 0 for {r:?}");
            assert!(r.avg_ms > 0.0, "avg_ms > 0 for {r:?}");
            assert!(r.med_ms > 0.0, "med_ms > 0 for {r:?}");
            assert_eq!(r.frames, 4, "frames-per-cell override honoured for {r:?}");
        }
    }
}
