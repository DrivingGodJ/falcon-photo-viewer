//! Shared preflight for every rotation apply entry point.
use std::{fs::OpenOptions, io, path::Path};

fn denied(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("read-only rotation target: {}", path.display()),
    )
}

fn writable_file(path: &Path) -> io::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().readonly() {
        return Err(denied(path));
    }
    // Check effective permissions/ACLs and sharing locks without changing any bytes.
    OpenOptions::new().read(true).write(true).open(path)?;
    Ok(())
}

#[cfg(unix)]
fn writable_folder(path: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    unsafe extern "C" {
        fn access(path: *const std::ffi::c_char, mode: std::ffi::c_int) -> std::ffi::c_int;
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().readonly() {
        return Err(denied(path));
    }
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "folder contains NUL"))?;
    // W_OK | X_OK: sidecar creation needs write and search access. This also
    // rejects read-only mounts, without creating a probe file on network shares.
    if unsafe { access(path.as_ptr(), 2 | 1) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn writable_folder(path: &Path) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    if !std::fs::metadata(path)?.is_dir() {
        return Err(denied(path));
    }
    // Windows directory READONLY is not an ACL. Probe actual creation using a
    // unique empty file; create_new never truncates an existing file.
    let probe = path.join(format!(
        ".falcon-rotate-probe-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    drop(file);
    std::fs::remove_file(probe)
}

/// Check all sources, their parents and existing matching sidecars before any
/// rotation write. `folder` optionally adds the displayed folder's permissions.
/// Call off the UI thread; a failed check prevents both sides of a paired shot
/// from being changed. Actual writes still handle permissions changing later.
pub fn check_rotation_write_access(
    folder: Option<&Path>,
    finished: Option<&Path>,
    raw: Option<&Path>,
) -> io::Result<()> {
    if let Some(folder) = folder {
        writable_folder(folder)?;
    }
    for (path, is_raw) in [(finished, false), (raw, true)] {
        let Some(path) = path else { continue };
        writable_file(path)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        writable_folder(parent)?;
        let sidecar = super::sidecar_path_for(path, is_raw);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => writable_file(&sidecar)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
