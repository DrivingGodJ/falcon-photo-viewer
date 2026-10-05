//! Preflight the destinations Apply will actually write, without creating probe files.
use super::*;
use std::{fs::{self, File, OpenOptions}, io};

pub(super) enum JpegRoute { Patch, AlreadyTarget, Sidecar(PatchErr) }

fn at(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}
fn denied(path: &Path) -> io::Error {
    at(path, io::Error::new(io::ErrorKind::PermissionDenied, "rotation target is read-only"))
}
fn writable_file(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path).map_err(|e| at(path, e))?;
    if !metadata.is_file() || metadata.permissions().readonly() { return Err(denied(path)); }
    OpenOptions::new().read(true).write(true).open(path).map_err(|e| at(path, e))?;
    Ok(())
}
fn writable_folder(path: &Path) -> io::Result<()> {
    if !fs::metadata(path).map_err(|e| at(path, e))?.is_dir() {
        return Err(at(path, io::Error::new(io::ErrorKind::NotADirectory, "not a directory")));
    }
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        unsafe extern "C" { fn access(path: *const std::ffi::c_char, mode: std::ffi::c_int) -> std::ffi::c_int; }
        if fs::metadata(path)?.permissions().readonly() { return Err(denied(path)); }
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| at(path, io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL")))?;
        if unsafe { access(name.as_ptr(), 2 | 1) } != 0 { return Err(at(path, io::Error::last_os_error())); }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_ADD_FILE on an existing directory, FILE_FLAG_BACKUP_SEMANTICS.
        // This checks the ACL without creating OneDrive/network sync activity.
        OpenOptions::new().access_mode(0x0002).custom_flags(0x0200_0000)
            .open(path).map_err(|e| at(path, e))?;
    }
    Ok(())
}
fn writable_sidecar(path: &Path) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    writable_folder(parent)?;
    match fs::symlink_metadata(path) {
        Ok(_) => writable_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(at(path, e)),
    }
}
fn jpeg_route(path: &Path, expected: u8, target: u8) -> io::Result<JpegRoute> {
    let mut file = File::open(path).map_err(|e| at(path, e))?;
    let cap = JPEG_LOCATE_CAP.min(file.metadata()?.len() as usize);
    let mut bytes = vec![0; cap];
    read_full(&mut file, &mut bytes).map_err(|e| at(path, e))?;
    let loc = match locate_jpeg_orientation(&bytes) {
        Ok(loc) => loc,
        Err(error) => return Ok(JpegRoute::Sidecar(PatchErr::Locate(error))),
    };
    file.seek(SeekFrom::Start(loc.value_offset)).map_err(|e| at(path, e))?;
    let mut value = [0; 2];
    read_full(&mut file, &mut value).map_err(|e| at(path, e))?;
    let found = loc.endian.u16(&value);
    Ok(if found == target as u16 { JpegRoute::AlreadyTarget }
        else if found == expected as u16 { JpegRoute::Patch }
        else { JpegRoute::Sidecar(PatchErr::Cas { expected, target, found }) })
}
pub(super) fn check_rotation_write_access(plan: &RotApplyPlan) -> io::Result<Option<JpegRoute>> {
    let mut route = None;
    for (path, raw) in [(plan.finished.as_deref(), false), (plan.raw.as_deref(), true)] {
        let Some(path) = path else { continue };
        if !fs::metadata(path).map_err(|e| at(path, e))?.is_file() {
            return Err(at(path, io::Error::new(io::ErrorKind::InvalidInput, "source is not a file")));
        }
        if !raw && plan.finished_is_jpeg {
            let expected = turns_to_orientation(plan.base_turns);
            let target = compose_exif_orientation(expected, plan.delta & 3);
            let jpeg = jpeg_route(path, expected, target)?;
            match &jpeg {
                JpegRoute::Patch => writable_file(path)?,
                JpegRoute::Sidecar(_) => writable_sidecar(&sidecar_fullname(path))?,
                JpegRoute::AlreadyTarget => {},
            }
            route = Some(jpeg);
        } else {
            // Never require write access to RAW/PNG/HEIC/TIFF originals.
            File::open(path).map_err(|e| at(path, e))?;
            writable_sidecar(&sidecar_path_for(path, raw))?;
        }
    }
    Ok(route)
}
