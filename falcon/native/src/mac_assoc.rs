//! v0.9.16 (Round B assoc): macOS **LaunchServices default-handler machinery** — the Settings
//! FILE ASSOCIATIONS card's Mac backend and the first-launch popup's apply path.
//!
//! ── WHAT IT DOES ──────────────────────────────────────────────────────────────────────────────────
//! The .app's Info.plist (scripts/mac-bundle.sh) *declares* every supported type at LSHandlerRank
//! Alternate — that alone only puts Falcon in Finder's "Open With" list. Making Falcon (or anyone)
//! the DOUBLE-CLICK default is a per-user LaunchServices preference, written here via
//! `LSSetDefaultRoleHandlerForContentType` per UTI (the CG-idiom precedent: raw `extern "C"` to the
//! OS frameworks, NO new crates).
//!
//! ── RUNTIME UTI RESOLUTION (why no static UTI table is trusted here) ──────────────────────────────
//! Every LS call needs a UTI, and the ext→UTI mapping is the SYSTEM's to decide (system declarations
//! outrank our imported ones). So each family's extensions are resolved LIVE via
//! `UTTypeCreatePreferredIdentifierForTag` — the answer is correct by construction on every macOS
//! version, whatever Apple declares this year. The static `support::MAC_FAMILY_UTIS` table is the
//! plist's evidence record + drift pin, NOT this module's input. (Before the .app has ever launched,
//! an import-only ext (e.g. .x3f) resolves to a `dyn.*` UTI — still a real UTI; LS preferences on it
//! bind to that exact extension, so the calls remain meaningful.)
//!
//! ── CF OWNERSHIP (audited per call) ───────────────────────────────────────────────────────────────
//! Create/Copy → we release: `CFStringCreateWithBytes`, `UTTypeCreatePreferredIdentifierForTag`,
//! `LSCopyDefaultRoleHandlerForContentType`, `LSCopyAllRoleHandlersForContentType` (the ARRAY is
//! released; its elements are borrowed via `CFArrayGetValueAtIndex` and NOT released). Everything
//! else is a borrow. All calls run on the UI thread (Settings callbacks / the popup choice).
//!
//! ── DEPRECATION NOTE ──────────────────────────────────────────────────────────────────────────────
//! `LSSetDefaultRoleHandlerForContentType` / `LSCopyDefaultRoleHandlerForContentType` are marked
//! deprecated since macOS 12 in favour of NSWorkspace's async `setDefaultApplication…` — but they
//! remain functional (and are the only SYNCHRONOUS path, which the read-back line needs). Every set
//! call's OSStatus is checked and logged by the caller; a future OS that hard-fails them degrades to
//! "the default didn't change", never a crash.

use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSFileManager, NSString};
use std::ffi::{c_char, c_void, CStr};

type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;

/// kLSRolesAll — the double-click default is the ALL-roles handler (Viewer-only preferences don't
/// move Finder's open verb).
const KLS_ROLES_ALL: u32 = 0xFFFF_FFFF;
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8 (the mac_colorsync constant)

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    // Copy → release. Null when no default is recorded (the OS would fall back to its built-in pick).
    fn LSCopyDefaultRoleHandlerForContentType(content_type: CFStringRef, role: u32) -> CFStringRef;
    // OSStatus (0 = noErr). Per-user preference write; takes effect in Finder immediately.
    fn LSSetDefaultRoleHandlerForContentType(
        content_type: CFStringRef,
        role: u32,
        handler_bundle_id: CFStringRef,
    ) -> i32;
    // Copy → release (the array; elements are the array's). Null when no app claims the type.
    fn LSCopyAllRoleHandlersForContentType(content_type: CFStringRef, role: u32) -> CFArrayRef;
    // Create → release. Never null for a valid tag (falls back to a dyn.* identifier).
    fn UTTypeCreatePreferredIdentifierForTag(
        tag_class: CFStringRef,
        tag: CFStringRef,
        conforming_to: CFStringRef,
    ) -> CFStringRef;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    // Signatures for shared symbols match support.rs's existing declarations EXACTLY
    // (clashing_extern_declarations stays quiet).
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        is_external: u8,
    ) -> CFStringRef;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFArrayGetCount(arr: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(arr: CFArrayRef, idx: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Rust str → owned CFString (Create → caller releases). Null only on allocation failure.
unsafe fn cfstr(s: &str) -> CFStringRef {
    CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0)
}

/// Borrow-read a CFString into an owned Rust String (512-byte buffer — bundle ids / UTIs are short).
/// Does NOT release `s` (ownership stays with the caller). None on null/failed conversion.
unsafe fn cf_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let mut buf = [0i8; 512];
    if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) == 0 {
        return None;
    }
    let out = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
    (!out.is_empty()).then_some(out)
}

/// Resolve a family's extensions to their LIVE preferred UTIs (deduped, order-preserving). The
/// system's mapping wins by construction; an undeclared ext yields its `dyn.*` identifier.
pub(crate) fn resolve_utis(exts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(exts.len());
    unsafe {
        let tag_class = cfstr("public.filename-extension");
        if tag_class.is_null() {
            return out;
        }
        for ext in exts {
            let tag = cfstr(ext);
            if tag.is_null() {
                continue;
            }
            let uti = UTTypeCreatePreferredIdentifierForTag(tag_class, tag, std::ptr::null());
            CFRelease(tag);
            if let Some(u) = cf_to_string(uti) {
                if !out.contains(&u) {
                    out.push(u);
                }
            }
            if !uti.is_null() {
                CFRelease(uti);
            }
        }
        CFRelease(tag_class);
    }
    out
}

/// The current default handler's bundle id for one UTI (None = no per-user preference recorded).
pub(crate) fn default_handler(uti: &str) -> Option<String> {
    unsafe {
        let cf_uti = cfstr(uti);
        if cf_uti.is_null() {
            return None;
        }
        let handler = LSCopyDefaultRoleHandlerForContentType(cf_uti, KLS_ROLES_ALL);
        CFRelease(cf_uti);
        let out = cf_to_string(handler);
        if !handler.is_null() {
            CFRelease(handler);
        }
        out
    }
}

/// Every app claiming one UTI (bundle ids). Empty when LS knows no claimant (or on any nil step).
pub(crate) fn all_handlers(uti: &str) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let cf_uti = cfstr(uti);
        if cf_uti.is_null() {
            return out;
        }
        let arr = LSCopyAllRoleHandlersForContentType(cf_uti, KLS_ROLES_ALL);
        CFRelease(cf_uti);
        if arr.is_null() {
            return out;
        }
        let n = CFArrayGetCount(arr);
        for i in 0..n {
            // Borrowed element (a CFString owned by the array) — read, never released here.
            if let Some(s) = cf_to_string(CFArrayGetValueAtIndex(arr, i)) {
                out.push(s);
            }
        }
        CFRelease(arr);
    }
    out
}

/// Set one UTI's default handler. Returns the OSStatus (0 = noErr) — the caller logs it.
pub(crate) fn set_default(uti: &str, bundle_id: &str) -> i32 {
    if !crate::mac_experiment::associations_allowed() {
        crate::support::log_event("mac-experiment: default-app change blocked in diagnostic build");
        return -1;
    }
    unsafe {
        let cf_uti = cfstr(uti);
        let cf_id = cfstr(bundle_id);
        if cf_uti.is_null() || cf_id.is_null() {
            if !cf_uti.is_null() {
                CFRelease(cf_uti);
            }
            if !cf_id.is_null() {
                CFRelease(cf_id);
            }
            return -1; // allocation failure — treated as a failed set
        }
        let status = LSSetDefaultRoleHandlerForContentType(cf_uti, KLS_ROLES_ALL, cf_id);
        CFRelease(cf_uti);
        CFRelease(cf_id);
        status
    }
}

/// Set `bundle_id` as the default for EVERY resolved UTI of a family. Returns
/// (utis_set_ok, first_error_status) — partial success is reported honestly (the caller's log line
/// carries both counts).
pub(crate) fn family_set_default(exts: &[&str], bundle_id: &str) -> (usize, i32) {
    let mut ok = 0usize;
    let mut first_err = 0i32;
    for uti in resolve_utis(exts) {
        let status = set_default(&uti, bundle_id);
        if status == 0 {
            ok += 1;
        } else if first_err == 0 {
            first_err = status;
        }
    }
    (ok, first_err)
}

/// The family's CURRENT default bundle id — read off exts[0]'s UTI (the Windows exts[0] probe rule:
/// one representative per family keeps read-back stable and cheap).
pub(crate) fn family_current(exts: &[&str]) -> Option<String> {
    let utis = resolve_utis(&exts[..1.min(exts.len())]);
    utis.first().and_then(|u| default_handler(u))
}

/// A bundle id → the app's user-facing display name ("Preview"), via NSWorkspace's app lookup +
/// NSFileManager's localized display name (strips ".app"). Falls back to the raw bundle id when the
/// app isn't installed / any step is nil — honest, still identifies the handler.
pub(crate) fn app_display_name(bundle_id: &str) -> String {
    unsafe {
        let ws = NSWorkspace::sharedWorkspace();
        let ns_id = NSString::from_str(bundle_id);
        if let Some(url) = ws.URLForApplicationWithBundleIdentifier(&ns_id) {
            if let Some(path) = url.path() {
                let fm = NSFileManager::defaultManager();
                let name = fm.displayNameAtPath(&path);
                let s = name.to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    bundle_id.to_string()
}
