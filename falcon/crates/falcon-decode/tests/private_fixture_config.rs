//! Private-default and public-portability policy, independent of the owner's files.
#[path = "../../test-support/fixture_paths.rs"]
mod fixture_paths;
use std::path::PathBuf;

#[test]
fn private_fixture_guard_is_host_scoped_and_explicitly_overridable() {
    assert!(fixture_paths::guard_for_host("windows", true));
    assert!(!fixture_paths::guard_for_host("windows", false));
    for host in [true, false] {
        assert!(fixture_paths::guard_for_host("1", host));
        assert!(!fixture_paths::guard_for_host("0", host));
        assert!(!fixture_paths::guard_for_host("", host));
    }
}
#[test]
fn portable_paths_override_windows_defaults_and_mac_ignores_windows_defaults() {
    let explicit = Some("portable-corpus".into()); let private = Some("private-corpus".into());
    for host in [true, false] {
        assert_eq!(fixture_paths::choose_override(explicit.clone(), private.clone(), host), Some(PathBuf::from("portable-corpus")));
    }
    assert_eq!(fixture_paths::choose_override(None, private.clone(), true), Some(PathBuf::from("private-corpus")));
    assert_eq!(fixture_paths::choose_override(None, private, false), None);
}
#[test]
fn required_corpus_cannot_be_missing_or_empty() {
    let root = std::env::temp_dir().join(format!("falcon-fixture-guard-{}-{}",std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    assert!(!root.exists());
    assert!(std::panic::catch_unwind(|| fixture_paths::check_directory(&root, true)).is_err());
    fixture_paths::check_directory(&root, false);
    std::fs::create_dir(&root).unwrap();
    let empty_rejected = std::panic::catch_unwind(|| fixture_paths::check_directory(&root, true)).is_err();
    std::fs::write(root.join("synthetic.bin"), b"synthetic fixture").unwrap();
    fixture_paths::check_directory(&root, true);
    assert!(root.canonicalize().unwrap().starts_with(std::env::temp_dir().canonicalize().unwrap()));
    std::fs::remove_file(root.join("synthetic.bin")).unwrap(); std::fs::remove_dir(&root).unwrap();
    assert!(empty_rejected);
}
