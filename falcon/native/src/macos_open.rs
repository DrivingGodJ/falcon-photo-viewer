//! macOS: the Apple-Events **"odoc"** (open-documents) delegate arm — v0.9.15 (Round B prerequisite
//! spike). It lets Finder **"Open With → Falcon"** and a double-click of an *associated* file reach
//! the app: macOS delivers those as an Apple Event / `application:openURLs:` message to the
//! **NSApplicationDelegate**, which winit owns. Before this arm the app received files ONLY via
//! `argv[1]` or a drag-drop onto the window (winit `DroppedFile`); Finder never uses argv for a
//! bundled `.app`, so associations were dead.
//!
//! ── ARCHITECTURE (winit 0.30.13 / Slint 1.17, read from the vendored sources) ─────────────────────
//! * winit installs its OWN delegate — a `declare_class!`-built NSObject subclass named
//!   `WinitApplicationDelegate` — via `app.setDelegate(..)` **once**, inside `EventLoop::new()`
//!   (`winit .../macos/event_loop.rs:240`). It NEVER re-asserts it (that is the only `setDelegate`
//!   call on the application in the whole backend). Slint drives that same delegate through winit's
//!   `ApplicationHandler`/`run_app` path, and `BackendSelector::select()` is what builds the event
//!   loop — so the delegate exists from boot, well before `app.run()`.
//! * That delegate implements only `applicationDidFinishLaunching:` / `applicationWillTerminate:`
//!   (`macos/app_state.rs:63`) — **never** `application:openURLs:`. So winit surfaces no open-files
//!   event and there is nothing to forward-to.
//! * winit's `send_event` swizzle calls `ApplicationDelegate::get(mtm)` on effectively every event,
//!   and `get()` does `delegate.is_kind_of::<WinitApplicationDelegate>()` and **panics** otherwise
//!   (`app.rs:45`, `app_state.rs:174-184`). So we must NOT replace the delegate with a proxy, and we
//!   must not disturb the delegate object's class identity.
//!
//! ── STRATEGY: extend the EXISTING class, do not replace the object ────────────────────────────────
//! We `class_addMethod` `application:openURLs:` onto the LIVE `WinitApplicationDelegate` **class**:
//! * winit never implements that selector, so the add always succeeds (nothing is overridden).
//! * the delegate OBJECT is untouched — same `isa`, same ivars — so winit's hot path
//!   (`is_kind_of` + `ivars()` on every event) is byte-for-byte unperturbed. This is the minimal
//!   perturbation; a subclass + `object_setClass` would work too (is_kind_of walks the chain) but
//!   needlessly changes the object's class.
//! * AppKit caches a delegate's `respondsToSelector:` answers at `setDelegate:` time, so after the
//!   add we **re-assign the same delegate** to force AppKit to re-scan — otherwise a launch-time
//!   "does not respond to openURLs" verdict would stick and the method would never be called.
//!
//! ── COLD-LAUNCH ORDERING (why no launch file is lost) ─────────────────────────────────────────────
//! When the app is launched BY opening a file, AppKit delivers `application:openURLs:` only after the
//! run loop starts pumping (it arrives around `applicationDidFinishLaunching:`, i.e. inside
//! `app.run()`). We inject on the main thread synchronously **before** `app.run()` (see
//! `install_odoc_handler`), so the method is provably in place before any odoc delivery — there is no
//! race. The handler hands the path to the UI thread's existing folder-open channel (drained by the
//! 16 ms tick → `begin_reload`), so a cold launch briefly shows the empty state and then opens the
//! delivered folder within a tick.
//!
//! ── DEFENSIVE POSTURE ─────────────────────────────────────────────────────────────────────────────
//! Every objc step is nil-/failure-checked; any miss logs `odoc: injection failed (<step>)` and the
//! app continues (associations simply won't open — never a crash). The callback body is wrapped in
//! `catch_unwind` because a Rust panic must not unwind across the ObjC frame (UB).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSArray, NSURL};

use crate::support::log_event;

/// File paths delivered by `application:openURLs:` — pushed by the objc callback (on the AppKit main
/// thread) and drained on the UI thread by the §tick, which forwards each into `begin_reload` (the
/// SAME channel argv / drag-drop use: a file → its parent folder, landing on the file via
/// `select_start`). A plain `Mutex<Vec<PathBuf>>` rather than the G7 picker channel, whose `PickKind`
/// is a `main`-local enum we cannot name here — `Mutex::new(Vec::new())` is const so this can be a
/// bare `static` (always `Sync`), and the lock is uncontended (both ends run on the main thread; the
/// Mutex only satisfies the `static` requirement).
pub(crate) static ODOC_PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The `application:openURLs:` implementation grafted onto winit's live delegate class. Runs on the
/// AppKit main thread. `catch_unwind` guards the ObjC boundary; every read is nil-checked.
///
/// Signature matches the ObjC method exactly — `void application:(NSApplication*)app
/// openURLs:(NSArray<NSURL*>*)urls` — so the raw `class_addMethod` type encoding is `"v@:@@"`.
extern "C" fn open_urls(
    _this: &AnyObject,
    _cmd: Sel,
    _app: &NSApplication,
    urls: &NSArray<NSURL>,
) {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let count = urls.count();
        let mut paths: Vec<PathBuf> = Vec::with_capacity(count);
        for i in 0..count {
            // `i < count`, so `objectAtIndex` is in bounds; `path()` is `None` for a non-file URL.
            let url = unsafe { urls.objectAtIndex(i) };
            if let Some(ns) = unsafe { url.path() } {
                paths.push(PathBuf::from(ns.to_string()));
            }
        }
        let first = paths
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<no file url>".to_string());
        // The one greppable line per event.
        log_event(&format!("odoc: {count} url(s) — {first}"));
        if !paths.is_empty() {
            // Hand off to the UI thread (drained by the tick → begin_reload).
            let mut q = ODOC_PENDING.lock().unwrap_or_else(|e| e.into_inner());
            q.extend(paths);
        }
    }));
    if run.is_err() {
        log_event("odoc: handler panicked (recovered)");
    }
}

/// Graft `application:openURLs:` onto winit's live application delegate. MUST be called on the main
/// thread, AFTER the Slint/winit backend exists (delegate created + set at `select()` during boot)
/// and BEFORE `app.run()` (so the method is in place before AppKit pumps any odoc event — the
/// cold-launch guarantee). Idempotent and fully defensive; on any miss it logs and returns.
pub(crate) fn install_odoc_handler() {
    let Some(mtm) = MainThreadMarker::new() else {
        log_event("odoc: injection failed (not main thread)");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // winit set this at `EventLoop::new()`; `None` would mean the backend has not been built yet
    // (it has, by boot's `select()`), so this arm is the belt-and-suspenders edge.
    let Some(delegate) = (unsafe { app.delegate() }) else {
        log_event("odoc: injection failed (no delegate)");
        return;
    };

    // The delegate's class == `WinitApplicationDelegate`. Both `ProtocolObject` and `AnyObject` are
    // repr-transparent wrappers over the same objc object pointer, so this reference cast is sound.
    let del_ref: &ProtocolObject<dyn NSApplicationDelegate> = &delegate;
    let del_obj: &AnyObject =
        unsafe { &*(del_ref as *const ProtocolObject<dyn NSApplicationDelegate> as *const AnyObject) };
    let cls: &AnyClass = del_obj.class();

    let sel = sel!(application:openURLs:);
    // Idempotent: if the class already carries the method (it won't, first call), skip the add.
    // `instance_method` walks the superclass chain; neither `WinitApplicationDelegate` nor its
    // supers (NSObject) implement openURLs, so first call this is `None`.
    if cls.instance_method(sel).is_some() {
        log_event("odoc: openURLs already present on the delegate class (skipping add)");
    } else {
        // Encoding: void return; self(@), _cmd(:), NSApplication*(@), NSArray*(@) → "v@:@@".
        let types = b"v@:@@\0";
        // Both are thin `extern "C"` fn pointers of identical size; ObjC will call it with the ABI
        // the encoding above declares (this is exactly how objc2's own `ClassBuilder` stores an IMP).
        let imp: objc2::ffi::IMP = Some(unsafe {
            std::mem::transmute::<
                extern "C" fn(&AnyObject, Sel, &NSApplication, &NSArray<NSURL>),
                unsafe extern "C" fn(),
            >(open_urls)
        });
        let added = unsafe {
            objc2::ffi::class_addMethod(
                cls as *const AnyClass as *mut objc2::ffi::objc_class,
                sel.as_ptr(),
                imp,
                types.as_ptr() as *const _,
            )
        };
        if !Bool::from_raw(added).as_bool() {
            log_event("odoc: injection failed (class_addMethod)");
            return;
        }
    }

    // Re-assign the (unchanged) delegate so AppKit refreshes its cached `respondsToSelector:` flags
    // and will actually dispatch `application:openURLs:` to our freshly-added method.
    app.setDelegate(Some(del_ref));
    log_event("odoc: openURLs handler installed on the winit app delegate");
    // Keep `delegate` alive to the end (it is only weakly held by NSApplication); winit's own strong
    // ref in `EventLoop` outlives us, but this makes the borrow explicit.
    let _keep: Retained<ProtocolObject<dyn NSApplicationDelegate>> = delegate;
}

// ── v0.9.61 (A1.5): `applicationShouldTerminate:` — the graft-independent quit door ──────────────
//
// WHY IT EXISTS. Everything that made ⌘Q graceful in v0.9.59 rides on the menu graft: muda's Quit
// item is retargeted to our controller, so the key equivalent enqueues a tag the tick drains. That
// retarget is only in place once the graft has taken, and it is only in place on the CURRENT bar —
// so between muda replacing the menu bar and our next tick re-grafting it, ⌘Q is muda's raw
// `terminate:` again, which kills the process inside AppKit and skips the whole flush tail. The
// same is true of every OTHER route into `terminate:`: the Dock's Quit item, `killall -TERM`-style
// polite quits, Apple-events quit, a restart/shut-down request. They all arrive HERE first.
//
// WHAT IT DOES: nothing but ask for the loop to end. It sets the latch the shutdown tail reads,
// raises the workers' cancel flags so an in-flight batch stops at its next file boundary, asks
// Slint to leave the loop, and answers `NSTerminateCancel` — "do not tear the process down; I am
// leaving under my own power". `app.run()` then returns into `main()`'s tail, which is the ONE
// tested exit; the tail's `Once` is what stops this door and the ⌘Q door from both running it.
//
// KNOWN LIMITATION (recorded here and in the round report; owner question Q-C'): a system LOGOUT or
// restart also arrives through this selector, and it receives our Cancel. macOS may therefore
// report Falcon as having interrupted the logout, even though the process exits a moment later of
// its own accord. The sanctioned shape for that case is `NSTerminateLater` + a
// `replyToApplicationShouldTerminate:` once the flush is done, which requires pumping a run loop
// from inside the callback — deliberately DEFERRED rather than half-built here.
//
// NO explicit hide-before-wait: Slint's generated `run()` hides the window before it returns (and
// `set_visibility` early-returns on a second call), so by the time the tail's bounded wait runs
// there is nothing on screen to look frozen.

/// Raised by the hook; read by the shutdown tail via [`terminate_was_requested`]. A plain static
/// because the callback is a bare `extern "C" fn` with no place to keep a handle.
static TERMINATE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The worker cancel latches the hook must raise (the copy/move file-boundary flag and the web
/// export's own cancel, which lives inside an `ExportProgress` rather than in a bare `Arc`). Held as
/// closures so the hook does not need to know either shape. `Mutex::new(Vec::new())` is const, so
/// this can be a bare `static`; registration and the hook both run on the main thread.
#[allow(clippy::type_complexity)]
static QUIT_CANCELS: Mutex<Vec<Box<dyn Fn() + Send>>> = Mutex::new(Vec::new());

/// Register a latch the terminate hook should raise. Called at boot, before `app.run()`.
pub(crate) fn register_quit_cancel(raise: Box<dyn Fn() + Send>) {
    QUIT_CANCELS.lock().unwrap_or_else(|e| e.into_inner()).push(raise);
}

/// Did `applicationShouldTerminate:` ask us to quit? The shutdown tail ORs this into the ⌘Q latch,
/// so a terminate-driven exit honours the same in-flight file-operation boundary.
pub(crate) fn terminate_was_requested() -> bool {
    TERMINATE_REQUESTED.load(Ordering::SeqCst)
}

/// NSApplicationTerminateReply::Cancel — "I will quit myself; do not kill me."
const NS_TERMINATE_CANCEL: usize = 0;

/// The grafted `applicationShouldTerminate:`. Runs on the AppKit main thread; `catch_unwind` guards
/// the ObjC boundary (a Rust panic must not unwind an ObjC frame — UB). Returns NSUInteger.
extern "C" fn should_terminate(_this: &AnyObject, _cmd: Sel, _app: &NSApplication) -> usize {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Idempotent: a second terminate request while we are already unwinding must not re-log or
        // re-ask; `swap` makes the first arrival the only one that does anything.
        if TERMINATE_REQUESTED.swap(true, Ordering::SeqCst) {
            return;
        }
        for raise in QUIT_CANCELS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            raise();
        }
        log_event(
            "terminate: applicationShouldTerminate — leaving the event loop for the graceful flush tail (answering NSTerminateCancel; a system logout may report this as an interruption — known limitation, see macos_open.rs)",
        );
        let _ = slint::quit_event_loop();
    }));
    NS_TERMINATE_CANCEL
}

/// Graft `applicationShouldTerminate:` onto winit's live application delegate — the same
/// `class_addMethod` technique [`install_odoc_handler`] uses, for the same reason (winit's hot path
/// does `is_kind_of` + `ivars()` on the delegate OBJECT, so the object must not be replaced).
///
/// VERIFIED against the vendored winit 0.30.13 sources: `WinitApplicationDelegate` declares
/// `applicationDidFinishLaunching:` and `applicationWillTerminate:` and NOTHING else
/// (`platform_impl/macos/app_state.rs`), so this selector is a pure ADD — nothing is overridden and
/// no winit behaviour is displaced. (`applicationWillTerminate:` IS implemented, which is why that
/// one is off the table: taking it would need swizzling.)
///
/// MUST be called on the main thread, after the backend exists and before `app.run()`.
pub(crate) fn install_terminate_handler() {
    let Some(mtm) = MainThreadMarker::new() else {
        log_event("terminate: injection failed (not main thread)");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = (unsafe { app.delegate() }) else {
        log_event("terminate: injection failed (no delegate)");
        return;
    };
    let del_ref: &ProtocolObject<dyn NSApplicationDelegate> = &delegate;
    let del_obj: &AnyObject =
        unsafe { &*(del_ref as *const ProtocolObject<dyn NSApplicationDelegate> as *const AnyObject) };
    let cls: &AnyClass = del_obj.class();

    let sel = sel!(applicationShouldTerminate:);
    if cls.instance_method(sel).is_some() {
        log_event("terminate: applicationShouldTerminate already present on the delegate class (skipping add)");
    } else {
        // Encoding: NSUInteger return (LP64 `unsigned long` → "L"); self(@), _cmd(:),
        // NSApplication*(@). The encoding is metadata for the runtime's introspection — the actual
        // call ABI comes from the fn pointer, and `usize` IS NSUInteger on every target this ships to.
        let types = b"L@:@\0";
        let imp: objc2::ffi::IMP = Some(unsafe {
            std::mem::transmute::<
                extern "C" fn(&AnyObject, Sel, &NSApplication) -> usize,
                unsafe extern "C" fn(),
            >(should_terminate)
        });
        let added = unsafe {
            objc2::ffi::class_addMethod(
                cls as *const AnyClass as *mut objc2::ffi::objc_class,
                sel.as_ptr(),
                imp,
                types.as_ptr() as *const _,
            )
        };
        if !Bool::from_raw(added).as_bool() {
            log_event("terminate: injection failed (class_addMethod) — a raw terminate: would bypass the flush tail");
            return;
        }
    }
    // Same reason as the odoc graft: AppKit caches `respondsToSelector:` at setDelegate: time.
    app.setDelegate(Some(del_ref));
    log_event("terminate: applicationShouldTerminate handler installed on the winit app delegate");
    let _keep: Retained<ProtocolObject<dyn NSApplicationDelegate>> = delegate;
}
