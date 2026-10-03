//! Dev probe (read-only): print what every candidate "which ICC is assigned to the monitor?" API
//! returns RIGHT NOW, so the stale-auto-detect bug (user changed the profile in Windows Settings,
//! Falcon's GetICMProfileW-based auto-detect kept returning the old one) can be root-caused
//! empirically. Compares: (1) GetICMProfileW on the screen DC (the v0.6.0 implementation),
//! (2) the modern per-display association ColorProfileGetDisplayDefault for every active display
//! path × both scopes × the plain/ACM subtypes, (3) WcsGetDefaultColorProfile per device name.
//! Usage: cargo run -p falcon-native --example icm_probe (Windows-only; elsewhere it compiles to
//! this stub)

#[cfg(windows)]
use windows::core::{PCWSTR, PWSTR};
#[cfg(windows)]
use windows::Win32::Devices::Display::{
    GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, QDC_ONLY_ACTIVE_PATHS,
};
#[cfg(windows)]
use windows::Win32::Foundation::LocalFree;
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
#[cfg(windows)]
use windows::Win32::UI::ColorSystem::{
    ColorProfileGetDisplayDefault, ColorProfileGetDisplayUserScope, GetICMProfileW,
    WcsGetDefaultColorProfile, COLORPROFILESUBTYPE, CPST_EXTENDED_DISPLAY_COLOR_MODE, CPST_NONE,
    CPST_STANDARD_DISPLAY_COLOR_MODE, CPT_ICC, WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
    WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
};

#[cfg(not(windows))]
fn main() {
    eprintln!("icm_probe probes the Windows ICM/WCS colour APIs; nothing to probe on this OS.");
}

#[cfg(windows)]
fn main() {
    // (1) the current v0.6.0 path: GetICMProfileW on the screen DC.
    unsafe {
        let dc = GetDC(None);
        let mut buf = vec![0u16; 512];
        let mut len: u32 = buf.len() as u32;
        let ok = GetICMProfileW(dc, &mut len, Some(PWSTR(buf.as_mut_ptr()))).as_bool();
        ReleaseDC(None, dc);
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        println!("[1] GetICMProfileW(screen DC)  ok={ok}  -> {}", String::from_utf16_lossy(&buf[..end]));
    }

    // (2) the modern per-display association, per active display path.
    unsafe {
        let mut n_paths = 0u32;
        let mut n_modes = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes).0 != 0 {
            println!("[2] GetDisplayConfigBufferSizes FAILED");
            return;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS, &mut n_paths, paths.as_mut_ptr(), &mut n_modes, modes.as_mut_ptr(), None,
        ).0 != 0 {
            println!("[2] QueryDisplayConfig FAILED");
            return;
        }
        for (i, p) in paths.iter().take(n_paths as usize).enumerate() {
            let luid = p.sourceInfo.adapterId;
            let sid = p.sourceInfo.id;
            println!("[2] path {i}: adapter LUID {}:{}  sourceId {sid}", luid.HighPart, luid.LowPart);
            match ColorProfileGetDisplayUserScope(luid, sid) {
                Ok(s) => println!("      user scope = {:?} (1=CURRENT_USER, 0=SYSTEM_WIDE)", s.0),
                Err(e) => println!("      user scope ERR {e:?}"),
            }
            for (scope, sname) in [
                (WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, "USER"),
                (WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE, "SYSTEM"),
            ] {
                for (sub, subname) in [
                    (CPST_NONE, "NONE"),
                    (CPST_STANDARD_DISPLAY_COLOR_MODE, "SDR/ACM-std"),
                    (CPST_EXTENDED_DISPLAY_COLOR_MODE, "HDR/ACM-ext"),
                ] as [(COLORPROFILESUBTYPE, &str); 3]
                {
                    match ColorProfileGetDisplayDefault(scope, luid, sid, CPT_ICC, sub) {
                        Ok(pw) => {
                            let s = pw.to_string().unwrap_or_default();
                            println!("      ColorProfileGetDisplayDefault {sname}/{subname} -> {s}");
                            let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(pw.as_ptr() as _)));
                        }
                        Err(e) => println!("      ColorProfileGetDisplayDefault {sname}/{subname} ERR 0x{:08x}", e.code().0),
                    }
                }
            }
        }
    }

    // (3) the WCS device-name API, both scopes, for \\.\DISPLAY1..3.
    unsafe {
        for d in 1..=3u32 {
            let dev: Vec<u16> = format!("\\\\.\\DISPLAY{d}").encode_utf16().chain(std::iter::once(0)).collect();
            for (scope, sname) in [
                (WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, "USER"),
                (WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE, "SYSTEM"),
            ] {
                let mut buf = vec![0u16; 512];
                let ok = WcsGetDefaultColorProfile(
                    scope, PCWSTR(dev.as_ptr()), CPT_ICC, CPST_NONE, 0,
                    (buf.len() * 2) as u32, PWSTR(buf.as_mut_ptr()),
                ).as_bool();
                if ok {
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
                    println!("[3] Wcs \\\\.\\DISPLAY{d} {sname} -> {}", String::from_utf16_lossy(&buf[..end]));
                }
            }
        }
    }
}
