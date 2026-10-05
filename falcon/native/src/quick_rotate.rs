//! Quick rotation's filesystem checks run off the UI thread. A result belongs to one
//! folder generation and photo; a slow network drive cannot enable a different photo.
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub generation: u64,
    pub index: usize,
    pub name: String,
    pub folder: PathBuf,
    pub finished: Option<PathBuf>,
    pub raw: Option<PathBuf>,
    pub gif: bool,
}

impl Target {
    /// A confirmation belongs to the captured photo, not a later folder/scan index.
    pub fn matches(&self, generation: u64, folder: &Path, shots: &[falcon_decode::Shot]) -> bool {
        shots
            .get(self.index)
            .and_then(|shot| Self::for_shot(generation, self.index, folder, shot))
            .as_ref()
            == Some(self)
    }

    pub fn for_shot(
        generation: u64,
        index: usize,
        folder: &Path,
        shot: &falcon_decode::Shot,
    ) -> Option<Self> {
        if shot.cloud_placeholder
            || (!shot.has_jpg && shot.raw.is_none())
            || folder.as_os_str().is_empty()
        {
            return None;
        }
        Some(Self {
            generation,
            index,
            name: shot.name.clone(),
            folder: folder.to_owned(),
            finished: shot.jpg.clone().filter(|_| shot.has_jpg),
            raw: shot.raw.clone(),
            gif: shot.has_jpg && shot.kind == falcon_decode::SrcKind::Gif,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Capability {
    Checking,
    Writable,
    ReadOnly,
    Unsupported,
}

/// Opening for write checks effective permissions/ACLs and locks without changing bytes.
fn file_writable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && !m.permissions().readonly())
        && OpenOptions::new().read(true).write(true).open(path).is_ok()
}

#[cfg(unix)]
fn folder_writable(path: &Path) -> bool {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn access(path: *const std::ffi::c_char, mode: std::ffi::c_int) -> std::ffi::c_int;
    }
    if !std::fs::metadata(path).is_ok_and(|m| m.is_dir() && !m.permissions().readonly()) {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // POSIX W_OK | X_OK: creating a sidecar needs write AND search access. access also
    // rejects read-only mounts. No probe files are created on Mac/Linux/network shares.
    unsafe { access(path.as_ptr(), 2 | 1) == 0 }
}

#[cfg(not(unix))]
fn folder_writable(path: &Path) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    if !std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    // A Windows directory's READONLY attribute isn't its ACL. Test actual creation,
    // off-thread, using a unique empty file; never truncate any existing file.
    let probe = path.join(format!(
        ".falcon-rotate-probe-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let Ok(file) = OpenOptions::new().write(true).create_new(true).open(&probe) else {
        return false;
    };
    drop(file);
    std::fs::remove_file(probe).is_ok()
}

pub(crate) fn check(target: &Target) -> Capability {
    if !folder_writable(&target.folder) {
        return Capability::ReadOnly;
    }
    let mut sources = 0;
    for (path, raw) in [
        (target.finished.as_ref(), false),
        (target.raw.as_ref(), true),
    ] {
        let Some(path) = path else {
            continue;
        };
        sources += 1;
        if !file_writable(path) || !path.parent().is_some_and(folder_writable) {
            return Capability::ReadOnly;
        }
        let sidecar = falcon_decode::sidecar_path_for(path, raw);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) if !file_writable(&sidecar) => return Capability::ReadOnly,
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Capability::ReadOnly,
            _ => {}
        }
    }
    if sources == 0 {
        return Capability::Unsupported;
    }
    if target.gif
        && !target
            .finished
            .as_ref()
            .is_some_and(|p| falcon_decode::gif_is_animated(p).is_ok_and(|animated| !animated))
    {
        return Capability::Unsupported;
    }
    Capability::Writable
}

/// Permission may have changed since the toolbar's last check. Recheck on the
/// write worker, before the existing lossless rotation writer touches a source.
pub(crate) fn apply_one(
    target: &Target,
    plan: &falcon_decode::RotApplyPlan,
) -> Result<falcon_decode::RotApplyReport, Capability> {
    let permission = check(target);
    if permission != Capability::Writable {
        return Err(permission);
    }
    Ok(falcon_decode::apply_rotation(plan))
}

pub(crate) struct Probe {
    requests: SyncSender<Target>,
    results: Receiver<(Target, Capability)>,
    target: Option<Target>,
    capability: Capability,
    last_requested: Option<Instant>,
}

impl Probe {
    pub fn new() -> Self {
        let (requests, work) = mpsc::sync_channel::<Target>(1);
        let (reply, results) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut target) = work.recv() {
                // Prefer the newest queued photo after a slow filesystem call.
                while let Ok(newer) = work.try_recv() {
                    target = newer;
                }
                let result = check(&target);
                if reply.send((target, result)).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            results,
            target: None,
            capability: Capability::Checking,
            last_requested: None,
        }
    }

    pub fn ready_for(&self, target: &Target) -> bool {
        self.target.as_ref() == Some(target) && self.capability == Capability::Writable
    }

    pub fn invalidate(&mut self) {
        self.target = None;
        self.capability = Capability::Checking;
        self.last_requested = None;
    }

    pub fn update(&mut self, target: Option<Target>, now: Instant) -> Capability {
        if self.target != target {
            self.target = target;
            self.capability = Capability::Checking;
            self.last_requested = None;
        }
        while let Ok((photo, capability)) = self.results.try_recv() {
            if self.target.as_ref() == Some(&photo) {
                self.capability = capability;
            }
        }
        if self.target.is_none() {
            self.capability = Capability::Unsupported;
        }
        if let Some(target) = &self.target {
            if self
                .last_requested
                .is_none_or(|then| now.saturating_duration_since(then) >= Duration::from_secs(2))
                && self.requests.try_send(target.clone()).is_ok()
            {
                self.last_requested = Some(now);
            }
        }
        self.capability
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "falcon-quick-rotate-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn target(&self) -> Target {
            let image = self.0.join("photo.png");
            std::fs::copy(
                concat!(env!("CARGO_MANIFEST_DIR"), "/assets/chrome-test.png"),
                &image,
            )
            .unwrap();
            Target {
                generation: 1,
                index: 0,
                name: "photo".into(),
                folder: self.0.clone(),
                finished: Some(image),
                raw: None,
                gif: false,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn read_only(path: &Path, yes: bool) {
        let mut p = std::fs::metadata(path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.set_mode(if yes { 0o444 } else { 0o644 });
        }
        #[cfg(not(unix))]
        p.set_readonly(yes);
        std::fs::set_permissions(path, p).unwrap();
    }
    #[test]
    fn confirmation_rejects_a_replaced_photo_folder_or_generation() {
        let f = Fixture::new();
        let target = f.target();
        let shot = falcon_decode::Shot {
            name: target.name.clone(),
            jpg: target.finished.clone(),
            has_jpg: true,
            kind: falcon_decode::SrcKind::Png,
            id: 0,
            has_raw: false,
            raw: None,
            sniffed: None,
            cloud_placeholder: false,
        };
        assert!(target.matches(1, &f.0, &[shot.clone()]));
        assert!(!target.matches(2, &f.0, &[shot.clone()]));
        assert!(!target.matches(1, &f.0.join("another-folder"), &[shot.clone()]));
        assert!(!target.matches(1, &f.0, &[]));
        let mut changed = shot;
        changed.jpg = Some(f.0.join("replacement.png"));
        assert!(!target.matches(1, &f.0, &[changed]));
    }

    #[test]
    fn checks_leave_photo_and_folder_unchanged() {
        let f = Fixture::new();
        let target = f.target();
        let image = target.finished.as_ref().unwrap();
        let before = std::fs::read(image).unwrap();
        assert_eq!(check(&target), Capability::Writable);
        assert_eq!(std::fs::read(image).unwrap(), before);
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 1);
    }

    #[test]
    fn jpeg_save_changes_only_the_orientation_value() {
        let f = Fixture::new();
        let mut target = f.target();
        let image = f.0.join("照片.jpg");
        // SOI, APP1/Exif, little-endian TIFF with one inline SHORT orientation,
        // next-IFD=0, EOI. The orientation value starts at byte 30.
        let bytes = vec![
            0xff, 0xd8, 0xff, 0xe1, 0, 34, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0,
            0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xd9,
        ];
        std::fs::write(&image, &bytes).unwrap();
        target.finished = Some(image.clone());
        let plan = falcon_decode::RotApplyPlan {
            finished: Some(image.clone()),
            finished_is_jpeg: true,
            raw: None,
            base_turns: 0,
            delta: 1,
        };
        let report = apply_one(&target, &plan).unwrap();
        assert!(report.ok);
        assert!(matches!(
            report.finished_action,
            falcon_decode::SideAction::Patched
        ));
        let mut expected = bytes;
        expected[30] = 6;
        assert_eq!(
            std::fs::read(image).unwrap(),
            expected,
            "no pixel bytes are re-encoded"
        );
    }

    #[test]
    fn foreign_sidecar_is_preserved_and_write_failure_is_reported() {
        let f = Fixture::new();
        let target = f.target();
        let image = target.finished.as_ref().unwrap();
        let sidecar = falcon_decode::sidecar_path_for(image, false);
        let foreign = b"private non-XMP metadata";
        std::fs::write(&sidecar, foreign).unwrap();
        let plan = falcon_decode::RotApplyPlan {
            finished: Some(image.clone()),
            finished_is_jpeg: false,
            raw: None,
            base_turns: 0,
            delta: 1,
        };
        assert!(!apply_one(&target, &plan).unwrap().ok);
        assert_eq!(std::fs::read(sidecar).unwrap(), foreign);
    }

    #[test]
    fn still_gif_can_rotate_but_animation_and_unreadable_gif_cannot() {
        let f = Fixture::new();
        let mut target = f.target();
        let gif = f.0.join("photo.gif");
        let header = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff";
        let frame = b"\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00";
        target.finished = Some(gif.clone());
        target.gif = true;
        for (frames, expected) in [(1, Capability::Writable), (2, Capability::Unsupported)] {
            let mut bytes = header.to_vec();
            for _ in 0..frames {
                bytes.extend_from_slice(frame);
            }
            bytes.push(0x3b);
            std::fs::write(&gif, bytes).unwrap();
            assert_eq!(check(&target), expected);
        }
        std::fs::write(&gif, b"bad GIF").unwrap();
        assert_eq!(check(&target), Capability::Unsupported);
    }
    #[test]
    fn rechecks_permissions_before_save_and_saves_only_the_requested_photo() {
        let f = Fixture::new();
        let target = f.target();
        let image = target.finished.as_ref().unwrap();
        let other = f.0.join("other.png");
        std::fs::copy(image, &other).unwrap();
        let bytes = std::fs::read(image).unwrap();
        let plan = falcon_decode::RotApplyPlan {
            finished: target.finished.clone(),
            finished_is_jpeg: false,
            raw: None,
            base_turns: 0,
            delta: 1,
        };
        assert_eq!(check(&target), Capability::Writable);
        read_only(image, true);
        assert!(matches!(
            apply_one(&target, &plan),
            Err(Capability::ReadOnly)
        ));
        assert!(!falcon_decode::sidecar_path_for(image, false).exists());
        read_only(image, false);
        assert!(apply_one(&target, &plan).unwrap().ok);
        assert_eq!(falcon_decode::sidecar_orientation(image, false), Some(6));
        assert_eq!(
            std::fs::read(image).unwrap(),
            bytes,
            "saving PNG direction preserves source pixels"
        );
        assert_eq!(std::fs::read(&other).unwrap(), bytes);
        assert!(!falcon_decode::sidecar_path_for(&other, false).exists());
        let plan = falcon_decode::RotApplyPlan {
            base_turns: 1,
            ..plan
        };
        assert!(apply_one(&target, &plan).unwrap().ok);
        assert_eq!(
            falcon_decode::sidecar_orientation(image, false),
            Some(3),
            "repeated clicks advance the saved direction"
        );
    }
    #[test]
    fn rejects_read_only_source_sidecar_and_missing_photo() {
        let f = Fixture::new();
        let target = f.target();
        let image = target.finished.as_ref().unwrap();
        read_only(image, true);
        assert_eq!(check(&target), Capability::ReadOnly);
        read_only(image, false);
        let sidecar = falcon_decode::sidecar_path_for(image, false);
        std::fs::write(&sidecar, b"xmp").unwrap();
        read_only(&sidecar, true);
        assert_eq!(check(&target), Capability::ReadOnly);
        read_only(&sidecar, false);
        std::fs::remove_file(image).unwrap();
        assert_eq!(check(&target), Capability::ReadOnly);
    }
    #[cfg(unix)]
    #[test]
    fn writable_photo_in_read_only_folder_is_disabled_and_recovers() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let target = f.target();
        std::fs::set_permissions(&f.0, std::fs::Permissions::from_mode(0o555)).unwrap();
        let denied = check(&target);
        std::fs::set_permissions(&f.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(denied, Capability::ReadOnly);
        assert_eq!(check(&target), Capability::Writable);
    }
    #[test]
    fn paired_raw_requires_permission_for_both_sources() {
        let f = Fixture::new();
        let mut target = f.target();
        let raw = f.0.join("photo.CR3");
        std::fs::write(&raw, b"test raw").unwrap();
        target.raw = Some(raw.clone());
        read_only(&raw, true);
        assert_eq!(check(&target), Capability::ReadOnly);
        read_only(&raw, false);
        assert_eq!(check(&target), Capability::Writable);
    }
    #[test]
    fn results_for_previous_photo_or_folder_never_enable_current_photo() {
        let (requests, _work) = mpsc::sync_channel(1);
        let (reply, results) = mpsc::channel();
        let f = Fixture::new();
        let a = f.target();
        let mut b = a.clone();
        b.generation += 1;
        let mut probe = Probe {
            requests,
            results,
            target: Some(a.clone()),
            capability: Capability::Writable,
            last_requested: None,
        };
        reply.send((a.clone(), Capability::Writable)).unwrap();
        assert_eq!(
            probe.update(Some(b.clone()), Instant::now()),
            Capability::Checking
        );
        assert!(!probe.ready_for(&a) && !probe.ready_for(&b));
        reply.send((b.clone(), Capability::Writable)).unwrap();
        probe.update(Some(b.clone()), Instant::now());
        assert!(probe.ready_for(&b));
        probe.invalidate();
        assert!(!probe.ready_for(&b));
    }
    #[test]
    fn bounded_probe_returns_without_waiting_for_a_filesystem_worker() {
        let (requests, _work) = mpsc::sync_channel(1);
        let (_reply, results) = mpsc::channel();
        let f = Fixture::new();
        let mut target = f.target();
        let mut probe = Probe {
            requests,
            results,
            target: None,
            capability: Capability::Checking,
            last_requested: None,
        };
        for _ in 0..100 {
            target.index += 1;
            assert_eq!(
                probe.update(Some(target.clone()), Instant::now()),
                Capability::Checking
            );
        }
        assert_eq!(probe.update(None, Instant::now()), Capability::Unsupported);
    }
}
