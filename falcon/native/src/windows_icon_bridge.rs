//! The portable executable carries its small, Windows-only icon helper. Explicit
//! association Update installs immutable bytes before advertising them to Explorer.
use std::{fs, io::{self, Write}, path::{Path, PathBuf}};

pub(crate) const CLSID: &str = "{BBABA60E-8FE6-4B64-8F05-AA306F973B4A}";
pub(crate) const CLASS_KEY: &str = r"Software\Classes\CLSID\{BBABA60E-8FE6-4B64-8F05-AA306F973B4A}";
pub(crate) const NAME: &str = env!("FALCON_ICON_HELPER_NAME");
const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/falcon-shell-icons.dll"));

fn publish_without_replacing(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" { fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32; }
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // Explicitly omit MOVEFILE_REPLACE_EXISTING. std::fs::rename DOES replace on Windows.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn deploy(directory: &Path) -> io::Result<PathBuf> {
    use windows::Win32::System::SystemInformation::{GetNativeSystemInfo, SYSTEM_INFO};
    let mut system = SYSTEM_INFO::default();
    unsafe { GetNativeSystemInfo(&mut system); }
    let native = unsafe { system.Anonymous.Anonymous.wProcessorArchitecture.0 };
    let built_for = if cfg!(target_arch = "x86_64") { 9 } else if cfg!(target_arch = "aarch64") { 12 } else { 0 };
    if native != built_for {
        return Err(io::Error::other("The icon helper needs a Falcon build matching Windows' native architecture"));
    }
    if !directory.is_absolute() { return Err(io::Error::other("icon helper directory must be absolute")); }
    fs::create_dir_all(directory)?;
    let destination = directory.join(NAME);
    match fs::read(&destination) {
        Ok(bytes) if bytes == BYTES => return Ok(destination),
        Ok(_) => return Err(io::Error::other("installed icon helper differs from this build")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let temp = directory.join(format!("{NAME}.{}.tmp", std::process::id()));
    // Never truncate somebody else's file, including an interrupted same-PID attempt.
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
    let result = (|| {
        file.write_all(BYTES)?;
        file.sync_all()?;
        drop(file);
        // Publish without replacing an existing destination. Parallel instances
        // can converge only if the winner published the exact same embedded bytes.
        if let Err(e) = publish_without_replacing(&temp, &destination) {
            if fs::read(&destination).ok().as_deref() != Some(BYTES) { return Err(e); }
        }
        if fs::read(&destination)?.as_slice() != BYTES {
            return Err(io::Error::other("could not verify installed icon helper"));
        }
        Ok(destination)
    })();
    let _ = fs::remove_file(temp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Falsifier: truncate/overwrite a mismatching helper instead of refusing to register it.
    #[test]
    fn immutable_deployment_reuses_exact_bytes_and_rejects_different_content() {
        let dir = std::env::temp_dir().join(format!("falcon-icon-deploy-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let path = deploy(&dir).unwrap();
        assert_eq!(fs::read(&path).unwrap(), BYTES);
        assert_eq!(deploy(&dir).unwrap(), path);
        fs::write(&path, b"unrelated bytes").unwrap();
        let racing = dir.join("racing.tmp");
        fs::write(&racing, BYTES).unwrap();
        assert!(publish_without_replacing(&racing, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unrelated bytes", "a raced-in file is never replaced");
        fs::remove_file(racing).unwrap();
        assert!(deploy(&dir).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unrelated bytes");
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
        assert!(deploy(Path::new("relative-directory")).is_err());
    }
}
