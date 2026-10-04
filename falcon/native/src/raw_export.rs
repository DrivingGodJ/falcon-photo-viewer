//! Per-run RAW export policy and progress. No rendering settings or image cache live here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::support::{write_export_manifest, ManifestOutcome, ManifestRec};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RawExportPolicy {
    #[default]
    Skip,
    CameraPreview,
    Develop,
}

impl RawExportPolicy {
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            1 => Self::CameraPreview,
            2 => Self::Develop,
            _ => Self::Skip,
        }
    }
}

// Keep the existing preview/finished export regressions unchanged. Production callers capture the
// explicit three-way policy; no persisted boolean can silently acquire the Develop meaning.
#[cfg(test)]
impl From<bool> for RawExportPolicy {
    fn from(include: bool) -> Self {
        if include {
            Self::CameraPreview
        } else {
            Self::Skip
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebPixelSource {
    Finished,
    CameraPreview,
    DevelopedRaw,
}

pub(crate) fn web_pixel_source(
    shot: &falcon_decode::Shot,
    policy: RawExportPolicy,
) -> Option<WebPixelSource> {
    if shot.is_unsupported() {
        None
    } else if shot.has_jpg {
        // The owner's RAW-only scope is independent of the viewer's RAW toggle.
        Some(WebPixelSource::Finished)
    } else if shot.raw.is_some() {
        match policy {
            RawExportPolicy::Skip => None,
            RawExportPolicy::CameraPreview => Some(WebPixelSource::CameraPreview),
            RawExportPolicy::Develop => Some(WebPixelSource::DevelopedRaw),
        }
    } else {
        None
    }
}

/// Language packs (round 2): marked here, translated where the tick publishes it.
pub(crate) const RAW_DEVELOP_CAPTION: &str =
    tr_noop!("Uses Falcon’s RAW development; colours may differ from the camera preview.");

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExportStatus {
    pub(crate) filename: String,
    pub(crate) phase: &'static str,
}

/// Coalesce retries by destination filename, preserving the manifest merge's order semantics.
/// Repeated failed writes cannot accumulate another full batch of duplicate records in memory.
#[derive(Default)]
struct PendingManifest {
    outcomes: std::collections::BTreeMap<String, (ManifestOutcome, Option<ManifestRec>)>,
    dirty: bool,
}

impl PendingManifest {
    fn record(&mut self, name: String, outcome: ManifestOutcome, rec: Option<ManifestRec>) {
        self.dirty = true;
        match outcome {
            ManifestOutcome::Ok | ManifestOutcome::Failed => {
                self.outcomes.insert(name, (outcome, rec));
            }
            // A web Skip never certifies new bytes. In particular it cannot erase a successful
            // prior run whose manifest write is still awaiting retry.
            ManifestOutcome::Skip | ManifestOutcome::Attention => {}
        }
    }

    fn flush(&self, dir: &std::path::Path) -> bool {
        if !self.dirty {
            return true;
        }
        let records: Vec<_> = self
            .outcomes
            .iter()
            .map(|(name, (outcome, rec))| (name.clone(), *outcome, *rec))
            .collect();
        write_export_manifest(dir, &records)
    }
}

#[derive(Default)]
pub(crate) struct CompletedWebExports {
    dir: Option<PathBuf>,
    pending: std::collections::BTreeMap<PathBuf, PendingManifest>,
    closed: bool,
}

impl CompletedWebExports {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn record(&mut self, name: String, rec: Option<ManifestRec>) {
        if let Some(dir) = self.dir.as_ref() {
            if let Some(batch) = self.pending.get_mut(dir) {
                batch.record(name, ManifestOutcome::Ok, rec);
            }
        }
    }
}

#[cfg(test)]
type BeforePublishHook = Box<dyn FnOnce(&ExportProgress) + Send>;

/// The UI reads status and changes Cancel without taking the publication lock. That lock covers
/// only destination publication + its completed-record append, never RAW decode or encoding.
#[derive(Default)]
pub(crate) struct ExportProgress {
    pub(crate) running: AtomicBool,
    pub(crate) cancel: AtomicBool,
    pub(crate) done: AtomicUsize,
    pub(crate) total: AtomicUsize,
    status: Mutex<ExportStatus>,
    completed: Mutex<CompletedWebExports>,
    #[cfg(test)]
    pub(crate) before_publish: Mutex<Option<BeforePublishHook>>,
}

impl ExportProgress {
    pub(crate) fn set_status(&self, filename: &str, phase: &'static str) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = ExportStatus {
            filename: filename.into(),
            phase,
        };
    }

    pub(crate) fn status(&self) -> ExportStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn begin_manifest(&self, dir: PathBuf) {
        let mut completed = self.lock_publication();
        // Preserve failed writes from earlier runs, including runs in another folder.
        completed.pending.entry(dir.clone()).or_default();
        completed.dir = Some(dir);
        completed.closed = false;
    }

    pub(crate) fn lock_publication(&self) -> MutexGuard<'_, CompletedWebExports> {
        self.completed.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn run_before_publish_hook(&self) {
        let hook = self.before_publish.lock().unwrap().take();
        if let Some(hook) = hook {
            hook(self);
        }
    }

    pub(crate) fn finish_manifest(
        &self,
        records: &[(String, ManifestOutcome, Option<ManifestRec>)],
    ) {
        let mut completed = self.lock_publication();
        if !completed.closed {
            if let Some(dir) = completed.dir.clone() {
                if let Some(batch) = completed.pending.get_mut(&dir) {
                    for (name, outcome, rec) in records {
                        batch.record(name.clone(), *outcome, *rec);
                    }
                    if batch.flush(&dir) {
                        completed.pending.remove(&dir);
                    }
                }
            }
            // Closing publication and persisting records are separate facts. A transient sharing
            // or rename failure retains the pending outcomes for a later run or shutdown retry.
            completed.closed = true;
        }
    }

    /// Called after the event loop has exited, on every exit route. The cancel flag plus the same
    /// publication lock linearizes this snapshot against a completed rename. A slow RAW can remain
    /// in the decoder beyond the Mac ten-second boundary, but cannot publish after this flush.
    /// Pending names are coalesced with O(log N) updates; there are no per-file manifest rewrites.
    pub(crate) fn stop_and_flush_completed(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let mut completed = self.lock_publication();
        completed.closed = true;
        completed.pending.retain(|dir, batch| !batch.flush(dir));
    }
}

/// Every normal outcome (including skip/failure) completes one counted item. Cancellation in the
/// current item explicitly disarms this guard, so N/N means the last item really finished.
pub(crate) struct ExportItemDone<'a> {
    progress: &'a ExportProgress,
    completed: bool,
}

impl<'a> ExportItemDone<'a> {
    pub(crate) fn new(progress: &'a ExportProgress) -> Self {
        Self {
            progress,
            completed: true,
        }
    }
    pub(crate) fn cancelled(&mut self) {
        self.completed = false;
    }
}

impl Drop for ExportItemDone<'_> {
    fn drop(&mut self) {
        if self.completed && !std::thread::panicking() {
            self.progress.done.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
pub(crate) enum WebPixelError {
    Cancelled,
    Failed(String),
}

pub(crate) type WebDecodedPixels = Result<(falcon_decode::Pixels, u32, u32), WebPixelError>;

pub(crate) fn decode_web_pixels(
    shot: &falcon_decode::Shot,
    keep: falcon_decode::Keep,
    source: WebPixelSource,
    progress: &ExportProgress,
) -> WebDecodedPixels {
    if source == WebPixelSource::DevelopedRaw {
        falcon_decode::develop_raw_pixels_for_export(shot, keep, || {
            progress.cancel.load(Ordering::Relaxed)
        })
        .map_err(|e| {
            if e.is::<falcon_decode::RawExportCancelled>() {
                WebPixelError::Cancelled
            } else {
                WebPixelError::Failed(e.to_string())
            }
        })
    } else {
        falcon_decode::decode_full_pixels(shot, keep)
            .map_err(|e| WebPixelError::Failed(e.to_string()))
    }
}
