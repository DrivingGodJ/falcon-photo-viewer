//! Optional private image corpora. Nothing here is linked into the application.
//! Environment variables are read at test/probe runtime, never baked into a binary.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// Private Cargo config supplies Windows-only defaults; public exports omit that config.
// A caller may opt into required coverage on any host with the value "1".
pub fn guard_for_host(mode: &str, windows: bool) -> bool {
    match mode {
        "" | "0" => false,
        "1" => true,
        "windows" => windows,
        _ => panic!("FALCON_REQUIRE_PRIVATE_FIXTURES must be 0, 1 or windows"),
    }
}
pub fn required() -> bool {
    guard_for_host(&std::env::var("FALCON_REQUIRE_PRIVATE_FIXTURES").unwrap_or_default(), cfg!(windows))
}
pub fn require_available(available: bool, label: &str) -> bool {
    assert!(available || !required(), "required private fixture unavailable: {label}");
    available
}
// Metadata only: never open a cloud placeholder merely to satisfy the private test guard.
// Same platform flags as falcon-decode's file_is_cloud_placeholder; this helper is also
// included by probe crates that do not depend on falcon-decode.
pub fn local_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() { return false; }
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0 { return false; }
    }
    #[cfg(target_os = "macos")] {
        use std::os::macos::fs::MetadataExt;
        if meta.st_flags() & 0x40000000 != 0 { return false; }
    }
    true
}
pub fn require_file(path: &Path) -> bool {
    require_available(local_file(path), &path.display().to_string())
}
pub fn choose_override(explicit: Option<std::ffi::OsString>, private_windows: Option<std::ffi::OsString>, windows: bool) -> Option<PathBuf> {
    explicit.filter(|p| !p.is_empty()).or_else(|| {
        if windows { private_windows.filter(|p| !p.is_empty()) } else { None }
    }).map(PathBuf::from)
}
pub fn check_directory(path: &Path, strict: bool) {
    if !strict { return; }
    let entries = std::fs::read_dir(path).unwrap_or_else(|e| panic!("required private corpus {}: {e}", path.display()));
    let mut files = 0;
    for entry in entries {
        let entry = entry.expect("required private corpus entry must be readable");
        if entry.file_type().expect("required private corpus entry type").is_file() {
            assert!(local_file(&entry.path()), "required private corpus file is not locally available: {}", entry.path().display());
            files += 1;
        }
    }
    assert!(files > 0, "required private corpus is empty: {}", path.display());
}
fn directory(variable: &str, leaf: &str) -> PathBuf {
    let path = choose_override(std::env::var_os(variable), std::env::var_os(format!("{variable}_WINDOWS")), cfg!(windows)).unwrap_or_else(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let crates = if manifest.file_name().is_some_and(|n| n == "native") {
            manifest.parent().unwrap().join("crates")
        } else { manifest.parent().unwrap().to_path_buf() };
        crates.join("testdata/private").join(leaf)
    });
    check_directory(&path, required());
    path
}
macro_rules! corpus {
    ($name:ident, $var:literal, $leaf:literal) => {
        pub fn $name() -> &'static Path {
            static ROOT: OnceLock<PathBuf> = OnceLock::new();
            ROOT.get_or_init(|| directory($var, $leaf)).as_path()
        }
    };
}
corpus!(photos, "FALCON_PHOTO_TEST_DIR", "photos");
corpus!(heic, "FALCON_HEIC_TESTKIT", "heic");
corpus!(standard, "FALCON_STANDARD_TESTKIT", "standard");
corpus!(edge, "FALCON_EDGE_TESTKIT", "edge");
corpus!(other_raw, "FALCON_OTHER_RAW_TEST_DIR", "other-raw");

pub fn standard_raw() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    let path = FILE.get_or_init(|| standard().join("B000_HWU_7800.CR3")).as_path();
    require_file(path);
    path
}
pub fn sample_heic() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    let path = FILE.get_or_init(|| heic().join("IMG_1826.HEIC")).as_path();
    require_file(path);
    path
}
pub fn benchmark_jpeg() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| std::env::var_os("FALCON_NVJPEG_TEST_FILE").map(PathBuf::from)
        .unwrap_or_else(|| photos().join("HWU_7781.JPG"))).as_path()
}
