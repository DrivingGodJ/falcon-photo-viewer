//! Regression contracts for the shared argv/drop scan path and metadata-backed sort.
use super::*;

#[test]
fn partial_photo_edits_do_not_enable_whole_folder_or_file_operations() {
    let source = include_str!("main.rs");
    for callback in ["discard_rotations", "apply_rotations", "copy_picks", "move_rejects",
        "delete_recycle", "export_web", "open_export", "toggle_xmp_sync", "xmp_backfill_confirm"] {
        // Assert the production callback gate, so native menus and shortcut calls
        // cannot bypass a disabled Slint control. This is the owner-approved scope.
        let marker = format!("app.on_{callback}(move");
        let body = source.split_once(&marker).unwrap().1.split_once("{ return; }").unwrap().0;
        assert!(body.contains("a.get_opening_photo()"), "{callback} must wait for the folder");
        assert!(!body.contains("get_photo_open_edits"), "{callback} is not a per-photo edit");
    }
}

#[test]
fn latest_explicit_open_cancels_both_queued_and_running_old_results() {
    let request = AtomicU64::new(0);
    let pending = Mutex::new(None::<&str>);
    let b = support::begin_explicit_scan(&request, &pending);
    *pending.lock().unwrap() = Some("B");
    let a = support::begin_explicit_scan(&request, &pending);
    assert!(
        pending.lock().unwrap().is_none(),
        "reopen A must retire B even if B already finished"
    );
    // Exercise the actual worker publication helper, including reverse completion.
    for (id, name) in [(b, "B"), (a, "A"), (b, "late B")] {
        assert_eq!(
            support::publish_latest(&request, &pending, id, name),
            id == a
        );
    }
    assert_eq!(*pending.lock().unwrap(), Some("A"));
    // Internal post-op reloads do not call begin_explicit_scan and leave a pending
    // different-folder request eligible. This distinction was the F2 regression.
    assert_eq!(request.load(Ordering::Relaxed), a);
}

#[test]
fn startup_has_one_async_open_without_filesystem_probes() {
    // Integration wiring is part of the regression: the correct request helper is
    // useless if argv still scans inline, or the same-folder branch skips it.
    let source = include_str!("main.rs");
    let boot = source
        .split("    let launch_target =")
        .nth(1)
        .unwrap()
        .split("    // v0.8.85: the FOLDER-AWARE")
        .next()
        .unwrap();
    assert!(!boot.contains("scan_folder"));
    assert!(!boot.contains("sort_shots"));
    assert!(source.contains("slint::Timer::single_shot(Duration::ZERO, move || open(path))"));
    let open = source
        .split("    let begin_reload: Rc<dyn Fn(PathBuf)>")
        .nth(1)
        .unwrap();
    let open = open
        .split("    // ── v0.8.49: the position-chip SORT menu picks")
        .next()
        .unwrap();
    assert!(!open.contains("resolve_target("));
    assert!(
        open.find("support::begin_explicit_scan").unwrap() < open.find(".spawn(move ||").unwrap()
    );
    assert!(
        open.find(".spawn(move ||").unwrap()
            < open.find("support::resolve_existing_target").unwrap()
    );
}

#[test]
fn explicit_target_resolution_rejects_missing_files_and_keeps_exact_filename() {
    let dir = std::env::temp_dir().join(format!(
        "falcon-open-target-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("photo.name.PNG");
    std::fs::write(&path, []).unwrap();
    assert_eq!(
        support::resolve_existing_target(&dir).unwrap(),
        (dir.clone(), None)
    );
    assert_eq!(
        support::resolve_existing_target(&path).unwrap(),
        (dir.clone(), Some("photo.name.PNG".into()))
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        support::resolve_existing_target(&path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    std::fs::remove_dir(&dir).unwrap();
}

#[test]
fn scan_metadata_sorts_without_reopening_disappeared_files() {
    let dir = std::env::temp_dir().join(format!(
        "falcon-sort-snapshot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    for (name, size) in [("a.png", 32), ("b.png", 96), ("c.png", 64)] {
        let mut bytes = vec![0; size];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let scan = falcon_decode::scan_folder_with_metadata(&dir).unwrap();
    assert_eq!(scan.shots.len(), 3);
    for name in ["a.png", "b.png", "c.png"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    let mut shots = scan.shots;
    support::sort_scanned_shots(
        &mut shots,
        SortMethod::Size,
        true,
        &BTreeMap::new(),
        &scan.metadata,
    );
    assert_eq!(
        shots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["b", "c", "a"]
    );
    assert!(shots.iter().enumerate().all(|(i, s)| s.id == i));
    support::sort_scanned_shots(
        &mut shots,
        SortMethod::Name,
        false,
        &BTreeMap::new(),
        &scan.metadata,
    );
    assert_eq!(
        shots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
}
