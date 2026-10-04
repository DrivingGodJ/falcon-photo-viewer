//! v0.9.22: macOS **native menu bar** — the NSMenu realization of `menubar_model` (Option B plus
//! View → Sort, the owner's ruling).
//!
//! ── ROUTE DECISION (investigated first — recorded here + in the round report) ─────────────────────
//! Slint 1.17's `MenuBar` element DOES reach the native macOS bar (the winit backend's muda adapter,
//! `i-slint-backend-winit/muda.rs`), but it CANNOT express this round's hard requirements:
//! * the APP menu is hardcoded (`create_default_app_menu` — "Until we have menu roles") — no
//!   Settings…/Show Welcome Guide items there;
//! * no windowsMenu/helpMenu roles (no AppKit window list, no Help search field);
//! * muda's predefined Quit is a raw `terminate:` — it kills the process inside AppKit, so the
//!   post-`app.run()` settings/selection flush NEVER runs (the round's RED-class risk — and a LIVE
//!   bug in the pre-v0.9.22 default menu);
//! * a Slint `MenuItem` shortcut registers a LIVE muda accelerator — a bare cull key (P/X/1…5)
//!   would be intercepted by AppKit before text fields, the exact ruled-out trap.
//! → RAW NSMenu route, in the house style (objc2 + msg_send, zero new crates/features/externs).
//!
//! ── WHY A GRAFT (not a from-scratch main menu) ────────────────────────────────────────────────────
//! Slint's winit backend auto-installs muda's default menu bar at the FIRST window activation and
//! RE-ASSERTS it (`init_for_nsapp` → `setMainMenu:`) on EVERY activation (winitwindowadapter.rs
//! `activation_changed`, muda.rs `window_activation_changed`). The re-assert passes the SAME NSMenu
//! object each time, and the default-bar adapter never rebuilds (its property tracker is `None`), so
//! a menu we install ALONGSIDE would be clobbered on every focus change — but items grafted INTO
//! muda's NSMenu object persist. (`BackendSelector` exposes no `with_default_menu_bar(false)`, and
//! swapping the app's backend construction for the raw winit builder would touch the Windows path —
//! ruled out by the branch's cfg-gate mandate.) So: the tick polls until muda's bar exists, then
//! grafts:
//!
//! ── v0.9.59 (round-4 item S1) / v0.9.61 (A2): THE GRAFT IS OWNED, AND IT IS IDEMPOTENT ──────────
//! The tester's round-4 bar was EMPTY while the log said "installed (7 menus; graft=ok)". The holes,
//! all now closed (findings §A; ledger L30):
//! * `NSApp.mainMenu != nil` is not identity. muda's bar arrives at the FIRST WINDOW ACTIVATION —
//!   `MudaAdapter::setup_default_menu_bar()` in the winit backend's `activation_changed`, which is
//!   the ONLY `setMainMenu:` in the whole graph — so before it, mainMenu is genuinely nil and the
//!   v0.9.22 header's "non-nil ⇒ after first activation" inference had nothing to stand on.
//!   v0.9.59's replacement theory ("SOMETHING held the main menu before muda's bar arrived") is
//!   ALSO wrong, and the sources say so: nothing else sets it. What actually happens is the
//!   opposite in time — the adapter is owned by the WINDOW (`WinitWindowOrNone::HasWindow {
//!   muda_adapter }`), so a window re-creation builds a FRESH adapter with a FRESH NSMenu and the
//!   bar we grafted into is replaced AFTERWARDS. That is the A→B case the ownership re-check below
//!   exists for; the fingerprint gate's job is only to refuse the pre-first-activation nil/empty
//!   state, and — the second correction — it CANNOT discriminate muda's bar from any other bar,
//!   because every standard app menu carries a ⌘Q. It is a "this bar is built" test, not a "this
//!   bar is muda's" test, and it is not asked to be more.
//! * A one-shot installer of externally-owned state has no idea when the OS replaces it. Every tick
//!   now POINTER-compares `NSApp.mainMenu` against the object we grafted into (`graft_state`) and
//!   tears down + re-grafts on a mismatch — muda re-asserts the same object per activation, so a
//!   healthy bar never thrashes — AND (v0.9.61) probes that our own probe item is still a child of
//!   that bar, so a bar that is the same OBJECT but has been emptied underneath us is caught too.
//! * v0.9.61 (A2): THE GRAFT IS IDEMPOTENT AGAINST ITSELF. The top-level appends used to be
//!   unconditional, so an A→B→A replacement (or any re-graft into a bar that still holds our rows)
//!   duplicated the ENTIRE bar with live-dispatching frozen duplicates. Every top-level holder we
//!   add now carries [`TAG_TOPLEVEL`], and a graft SWEEPS them out of the target bar — in a loop,
//!   because one `indexOfItemWithTag:` removes one item — before appending anything. The
//!   app-submenu inserts sweep the same way, `setWindowsMenu:`/`setHelpMenu:` are cleared before
//!   the sweep so AppKit is not left pointing at menus we are about to remove, and a graft is
//!   deferred by one tick while a menu is being TRACKED (`highlightedItem != nil`) — rebuilding a
//!   menu bar under an open menu is not something AppKit documents surviving.
//! * "installed" is now printed only after a READ-BACK proves our menus are children of the LIVE
//!   `NSApp.mainMenu`; a failed read-back retries on the next tick. The line quotes the bar's LIVE
//!   `numberOfItems` and its top-level titles — v0.9.59 still printed a hardcoded "7 menus", which
//!   counted nothing and would have printed 7 over a 13-menu duplicated bar: the L30 defect,
//!   inside the L30 fix.
//!
//! The graft itself:
//! * app submenu (muda's): insert Settings… ⌘, + Show Welcome Guide after About+separator; RETARGET
//!   the existing Quit item (found by key equivalent "q") from `terminate:` to our action — ⌘Q then
//!   rides the queue → tick → `invoke_close_requested()` → the SAME guarded path as the title-bar ×
//!   (mid-op quit-when-idle, rotation reminder) → `slint::quit_event_loop()` → `app.run()` returns →
//!   the final `save_settings` + selection flush + writer barrier in main().
//! * append File · Edit · Photo · View · Window · Help, register windowsMenu + helpMenu.
//!
//! ── STATE + DISPATCH ARCHITECTURE (single dispatch path) ──────────────────────────────────────────
//! * Menu ACTIONS only enqueue the item's tag (`CMD_QUEUE`); the 16 ms tick drains and routes every
//!   tag to the SAME `invoke_*` callback the in-app control uses (main.rs, beside the odoc drain —
//!   the proven `ODOC_PENDING` pattern). No Slint call ever happens inside an AppKit callback.
//! * The tick pushes a fresh `MenuSnapshot` here every frame (`SNAPSHOT`); `validateMenuItem:` reads
//!   it synchronously — AppKit validates BOTH at menu-open and at key-equivalent time, so enablement
//!   is ≤16 ms fresh even for ⌘-chords. Titles/check state re-apply on fingerprint change AND on
//!   `menuNeedsUpdate:` (menu open). The File menu alone runs `autoenablesItems = NO` so the Open
//!   Recent PARENT can disable when the recents list is empty (AppKit auto-enables submenu parents).
//! * Chord double-dispatch: winit 0.30.13 implements NO `performKeyEquivalent` anywhere (verified by
//!   source grep) and its `sendEvent:` override only special-cases Cmd `keyUp`, so an ENABLED menu
//!   equivalent is consumed by AppKit's standard main-menu pass and never reaches winit → Slint. A
//!   DISABLED item's chord either falls through to the in-app Slint arm (which carries its own
//!   guards — the pre-menu behavior) or is swallowed; both are single-dispatch by construction.
//!
//! ── DEFENSIVE POSTURE (house rule) ────────────────────────────────────────────────────────────────
//! Every objc step is nil-checked; a graft miss logs `menubar: graft failed (<step>)` / `menubar:
//! quit retarget FAILED …` and degrades (menus partial, never a crash). All three ObjC callbacks are
//! `catch_unwind`-wrapped (a Rust panic must not unwind an ObjC frame). Controller/menus/items are
//! retained for the process lifetime in a main-thread `thread_local` (NSMenuItem.target and
//! NSMenu.delegate are WEAK — the strong refs here are load-bearing).
//!
//! Cannot run on this Windows host — verified by `cargo check --target aarch64-apple-darwin` (the
//! round's only compiler for this file) + the Windows-side model tests in `menubar_model.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use objc2::declare::ClassBuilder;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, Sel};
use objc2::{class, msg_send, msg_send_id, sel, ClassType};
use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSString};

use crate::i18n;
use crate::menubar_model::{
    app_menu_inserts, app_submenu_fingerprint_ok, build_menus, enabled_for, fingerprint,
    graft_state, should_attempt_graft, top_menus, GraftState, ItemDef, MenuSnapshot, TopMenu,
    TAG_QUIT, TAG_RECENT_BASE, TAG_RECENT_EMPTY, TAG_RECENT_PARENT,
};
use crate::support::log_event;

/// NSEventModifierFlagCommand / ...Control (AppKit constants — stable ABI values).
const MASK_CMD: usize = 1 << 20;
const MASK_CTRL: usize = 1 << 18;

/// Tags clicked in the menu, drained by the tick (the ODOC_PENDING pattern — both ends run on the
/// AppKit main thread; the Mutex only satisfies the `static` requirement).
pub(crate) struct MenuCommand {
    pub(crate) tag: i32,
    pub(crate) repeated: bool,
    pub(crate) context: Option<u64>,
}
static CMD_QUEUE: Mutex<Vec<MenuCommand>> = Mutex::new(Vec::new());

/// The tick-refreshed app-state snapshot `validateMenuItem:` / `menuNeedsUpdate:` read. `None`
/// only before the first tick.
static SNAPSHOT: Mutex<Option<MenuSnapshot>> = Mutex::new(None);

/// v0.9.59 (S1 ruling 3): the separator we insert into muda's app submenu carries a private tag so
/// a RE-graft into the same submenu object can remove its own previous one instead of stacking
/// separators. Model tags are positive; this can never collide with one.
const TAG_APP_SEP: isize = -9001;

/// v0.9.61 (A2): every TOP-LEVEL holder this graft appends to `NSApp.mainMenu` carries this tag —
/// File, Edit, Photo, View, the separately-built Window holder and Help alike. It is what makes a
/// re-graft idempotent no matter what muda did: the sweep removes every item wearing it before a
/// single append happens, so N grafts into one bar leave exactly the bar one graft leaves.
/// (The Window holder is the easy miss — it is built in its own branch above the shared loop, and
/// it was the one holder v0.9.59's teardown could never have reached.)
const TAG_TOPLEVEL: isize = -9002;

/// Everything the realized menu needs to stay alive + updatable. Main-thread only (AppKit).
struct MenuRefs {
    /// v0.9.59 (S1 ruling 1): the `NSApp.mainMenu` object this graft went into. Every tick compares
    /// it (pointer identity) against the live one — the ownership re-check L30 demands of anything
    /// installed into state the OS can replace.
    main_menu: *mut AnyObject,
    /// One item we appended, kept for the read-back: "is this still a child of the LIVE main menu?"
    probe_item: *mut AnyObject,
    /// v0.9.61 (A2): STRONG references behind the two raw pointers above (and behind the retargeted
    /// ⌘Q item). Pointer identity is the whole ownership argument — `graft_state` says "the bar is
    /// still ours" because an address matches — and an address only means identity while the object
    /// at it is alive. muda's bar is retained by muda, and the Quit item by that bar, so in practice
    /// they outlive us; but "in practice, because of another crate's allocation ordering" is not the
    /// same as "by construction", and a freed-and-reused address would make `Healthy` a lie in the
    /// one direction that cannot be noticed. Cheap belt: three retains for the process lifetime.
    /// (The skeptic pair split on whether this is necessary; the retain won as the cheaper mistake.)
    strong: Vec<Retained<AnyObject>>,
    /// tag → NSMenuItem for every STATIC tagged item (recents children are rebuilt dynamically).
    items: Vec<(i32, *mut AnyObject)>,
    /// Tags in the menus that run `autoenablesItems = NO` → their enabled state comes from
    /// `enabled_for` on every refresh, not from AppKit's validation walk.
    /// v0.8.119 (design-sweep Y57): View JOINED File here. The model has always computed a rule for
    /// the Sort PARENT (`TAG_SORT_PARENT => photo_ok`) and nothing could enforce it, because View
    /// autoenabled and AppKit enables a submenu parent regardless of validation — so on the app's
    /// debut state (no folder) "Sort ▸" read fully enabled and opened onto nine greyed rows. That is
    /// the identical case one menu over that File → Open Recent was given a whole mechanism for.
    explicit_tags: Vec<i32>,
    /// The Open Recent NSMenu (rebuilt from the snapshot on each of its menuNeedsUpdate:).
    recent_menu: *mut AnyObject,
    controller: *mut AnyObject,
    last_fp: u64,
    /// Strong refs for the process lifetime: controller, every NSMenu, every static NSMenuItem
    /// (targets/delegates are weak in AppKit — dropping these would dangle them).
    keep: Vec<Retained<AnyObject>>,
}

thread_local! {
    static REFS: RefCell<Option<MenuRefs>> = const { RefCell::new(None) };
    /// Ticks spent waiting for muda's bar — one diagnostic line if it never shows (~10 s).
    static WAIT_TICKS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// v0.9.59 (S1 ruling 3): the ONE controller object for the process lifetime. NSMenuItem.target
    /// and NSMenu.delegate are WEAK/unretained, and a re-graft leaves the PREVIOUS bar's items (in
    /// particular muda's retargeted ⌘Q) pointing at it, so it must outlive every MenuRefs — dropping
    /// it with the refs would leave AppKit sending `falconMenuAction:` to freed memory.
    static CONTROLLER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    /// The one-shot "this is what the bar actually looked like" diagnostic (S1 ruling 2).
    static FINGERPRINT_LOGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Re-graft counter — the field log proves the bar is LIVE, not merely once-installed.
    static REGRAFTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

// ── the ObjC controller (FalconMenuController: NSObject) ─────────────────────────────────────────

/// `falconMenuAction:` — every custom item's action. Enqueue the tag; nothing else (no Slint calls
/// inside AppKit callbacks — the tick drains + dispatches + logs). Receivers are raw pointers:
/// `&AnyObject` in a fn-pointer type is higher-ranked over its lifetime, which objc2 0.5's
/// `MethodImplementation` (implemented per concrete lifetime) rejects — `*mut AnyObject` is the
/// lifetime-free `MessageReceiver` form.
extern "C" fn action_cb(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let run = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        if sender.is_null() {
            return;
        }
        let tag: isize = msg_send![sender, tag];
        // Native key equivalents bypass Slint's KeyEvent. Carry their repeat bit
        // so a held Cmd+C/Cmd+Z cannot spill onto the global target after a menu closes.
        let repeated = MainThreadMarker::new().is_some_and(|mtm| {
            let app = NSApplication::sharedApplication(mtm);
            let event: *mut AnyObject = msg_send![&*app, currentEvent];
            if event.is_null() { return false; }
            let kind: usize = msg_send![event, type];
            if kind != 10 { return false; } // NSEventTypeKeyDown; isARepeat is key-event-only
            let repeat: Bool = msg_send![event, isARepeat];
            repeat.as_bool()
        });
        let context = SNAPSHOT.lock().unwrap_or_else(|e|e.into_inner()).as_ref().and_then(|s|s.context);
        {
            let mut q = CMD_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            q.push(MenuCommand { tag: tag as i32, repeated, context });
        }
        // v0.9.67 (F2 — the Wave-A verify's B-Y1): TELL THE TICK GOVERNOR. This queue is drained by
        // the tick, and since v0.9.66 the tick idles at 8 Hz when nothing is happening — and nothing
        // in the winit event stream announces a native menu-bar action, so the drain that services
        // this tag could be up to 125 ms out. Note activity + snap the timer back to full rate, the
        // same thing the winit event filter does before it lets a keypress proceed, so the tag we
        // just pushed is drained ≤16 ms from here. The lock is released FIRST (above): the snap
        // takes no lock of ours, but a callback that holds a mutex across an unrelated call is the
        // shape this file's own defensive posture avoids. See `crate::note_menu_activity`.
        crate::note_menu_activity();
    }));
    if run.is_err() {
        log_event("menubar: action handler panicked (recovered)");
    }
}

/// `validateMenuItem:` — AppKit calls this at menu-open AND key-equivalent time for every item
/// targeting the controller (autoenabled menus). Reads the ≤16 ms snapshot; the one enablement
/// predicate lives in the model (`enabled_for`).
extern "C" fn validate_cb(_this: *mut AnyObject, _cmd: Sel, item: *mut AnyObject) -> Bool {
    let ok = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        if item.is_null() {
            return true;
        }
        let tag: isize = msg_send![item, tag];
        let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(s) => enabled_for(tag as i32, s),
            None => true,
        }
    }))
    .unwrap_or(true);
    Bool::new(ok)
}

/// `menuNeedsUpdate:` (NSMenuDelegate, set on every menu WE created) — the on-OPEN refresh the
/// constraints require: recents rebuild for the Open Recent menu, title/check/File-enable re-apply
/// for everything else.
extern "C" fn needs_update_cb(_this: *mut AnyObject, _cmd: Sel, menu: *mut AnyObject) {
    let run = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let snap = {
            let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
            g.clone()
        };
        let Some(snap) = snap else { return };
        REFS.with(|r| {
            if let Some(refs) = r.borrow_mut().as_mut() {
                unsafe {
                    if !menu.is_null() && menu == refs.recent_menu {
                        rebuild_recents(refs, &snap);
                    } else {
                        apply_refresh(refs, &snap);
                    }
                }
            }
        });
    }));
    if run.is_err() {
        log_event("menubar: menuNeedsUpdate panicked (recovered)");
    }
}

/// Get-or-declare the controller class (idempotent — `get` first so a re-entry never re-registers).
fn controller_class() -> Option<&'static AnyClass> {
    if let Some(c) = AnyClass::get("FalconMenuController") {
        return Some(c);
    }
    let mut b = ClassBuilder::new("FalconMenuController", NSObject::class())?;
    unsafe {
        b.add_method(
            sel!(falconMenuAction:),
            action_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        b.add_method(
            sel!(validateMenuItem:),
            validate_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
        );
        b.add_method(
            sel!(menuNeedsUpdate:),
            needs_update_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
    }
    Some(b.register())
}

/// v0.9.59 (S1 ruling 3): the process-lifetime controller. Created on first use, NEVER dropped —
/// see the CONTROLLER thread_local for why a re-graft must not take it with it.
unsafe fn controller_ptr() -> Option<*mut AnyObject> {
    CONTROLLER.with(|c| {
        let mut b = c.borrow_mut();
        if b.is_none() {
            let cls = controller_class()?;
            let obj: Retained<AnyObject> = msg_send_id![cls, new];
            *b = Some(obj);
        }
        b.as_ref().map(|o| Retained::as_ptr(o) as *mut AnyObject)
    })
}

// ── NSMenu inspection helpers (v0.9.59 S1: the graft target is now READ before it is written) ────

/// An NSMenuItem's `title` / `keyEquivalent` as a Rust String ("" for nil — never a panic).
unsafe fn item_string(item: *mut AnyObject, which: bool) -> String {
    if item.is_null() {
        return String::new();
    }
    let s: *mut AnyObject = if which { msg_send![item, title] } else { msg_send![item, keyEquivalent] };
    if s.is_null() {
        String::new()
    } else {
        (*(s as *const NSString)).to_string()
    }
}

/// (title, keyEquivalent) for every item of a menu — the fingerprint input AND the one-shot
/// diagnostic that settles the field's remaining ordering-vs-recreation unknown.
unsafe fn menu_rows(menu: *mut AnyObject) -> Vec<(String, String)> {
    if menu.is_null() {
        return Vec::new();
    }
    let n: isize = msg_send![menu, numberOfItems];
    (0..n)
        .map(|i| {
            let it: *mut AnyObject = msg_send![menu, itemAtIndex: i];
            (item_string(it, true), item_string(it, false))
        })
        .collect()
}

/// The live `NSApp.mainMenu` as a bare address (0 = nil) — the input to `graft_state`.
unsafe fn live_main_menu(app: &NSApplication) -> *mut AnyObject {
    msg_send![app, mainMenu]
}

/// v0.9.61 (A2): is a menu currently being TRACKED (an open menu with a highlighted row)? AppKit
/// gives no contract for rebuilding a menu bar mid-tracking, and a re-graft removes and re-adds
/// top-level items — so a graft that becomes due while the user is holding a menu open waits one
/// tick. Menu tracking is a modal run loop; this is only ever true for as long as a menu is down.
unsafe fn menu_is_tracking(main_menu: *mut AnyObject) -> bool {
    if main_menu.is_null() {
        return false;
    }
    let hi: *mut AnyObject = msg_send![main_menu, highlightedItem];
    !hi.is_null()
}

/// v0.9.61 (A2): remove EVERY item of `menu` carrying `tag`, returning how many went. A loop,
/// because `indexOfItemWithTag:` answers with the FIRST match only — the v0.9.59 app-submenu sweep
/// removed one copy per tag and would have left the second of any pair standing, which is the same
/// one-item asymmetry the separator sweep next to it had already been written as a loop to avoid.
unsafe fn sweep_tag(menu: *mut AnyObject, tag: isize) -> usize {
    let mut n = 0usize;
    if menu.is_null() {
        return n;
    }
    loop {
        let idx: isize = msg_send![menu, indexOfItemWithTag: tag];
        if idx < 0 {
            return n;
        }
        let () = msg_send![menu, removeItemAtIndex: idx];
        n += 1;
        if n > 256 {
            return n; // defensive: never spin on a menu that refuses to shrink
        }
    }
}

/// The LIVE top-level titles of a menu bar — the read-back the "installed" line quotes, so the log
/// describes the bar AppKit is showing rather than the number of appends we made.
unsafe fn toplevel_titles(menu: *mut AnyObject) -> Vec<String> {
    menu_rows(menu).into_iter().map(|(t, _)| t).collect()
}

// ── NSMenu construction helpers (raw msg_send — no new crate features) ───────────────────────────

unsafe fn new_menu(title: &str) -> Retained<AnyObject> {
    let t = NSString::from_str(title);
    let alloc: Allocated<AnyObject> = msg_send_id![class!(NSMenu), alloc];
    msg_send_id![alloc, initWithTitle: &*t]
}

/// A tagged action item wired to the controller. `key` = "" for no equivalent; `ctrl` adds ⌃ to the
/// implicit ⌘ (the model pins that every live equivalent is a ⌘-chord).
unsafe fn new_action_item(def: &ItemDef, ctrl_obj: *mut AnyObject) -> Retained<AnyObject> {
    let t = NSString::from_str(&def.title);
    let k = NSString::from_str(def.key);
    let alloc: Allocated<AnyObject> = msg_send_id![class!(NSMenuItem), alloc];
    let item: Retained<AnyObject> =
        msg_send_id![alloc, initWithTitle: &*t, action: sel!(falconMenuAction:), keyEquivalent: &*k];
    let () = msg_send![&*item, setTarget: ctrl_obj];
    let () = msg_send![&*item, setTag: def.tag as isize];
    if !def.key.is_empty() {
        let mask = MASK_CMD | if def.ctrl { MASK_CTRL } else { 0 };
        let () = msg_send![&*item, setKeyEquivalentModifierMask: mask];
    }
    if def.checked {
        let () = msg_send![&*item, setState: 1isize];
    }
    item
}

unsafe fn new_separator() -> Retained<AnyObject> {
    msg_send_id![class!(NSMenuItem), separatorItem]
}

/// Realize one model item list into `menu`, registering static tagged items + retaining them.
/// Recents CHILDREN are deliberately not registered (rebuilt per open); the recent menu ptr is
/// captured for the delegate dispatch.
unsafe fn realize_into(
    menu: &AnyObject,
    defs: &[ItemDef],
    ctrl: *mut AnyObject,
    refs_items: &mut Vec<(i32, *mut AnyObject)>,
    keep: &mut Vec<Retained<AnyObject>>,
    recent_menu: &mut *mut AnyObject,
    in_recents: bool,
) {
    for def in defs {
        if def.separator {
            let sep = new_separator();
            let () = msg_send![menu, addItem: &*sep];
            continue;
        }
        let item = new_action_item(def, ctrl);
        if let Some(sub) = &def.submenu {
            let m = new_menu(&def.title);
            let () = msg_send![&*m, setDelegate: ctrl];
            let is_recents = def.tag == TAG_RECENT_PARENT;
            if is_recents {
                *recent_menu = Retained::as_ptr(&m) as *mut AnyObject;
            }
            realize_into(&m, sub, ctrl, refs_items, keep, recent_menu, is_recents);
            let () = msg_send![&*item, setSubmenu: &*m];
            keep.push(m);
        }
        let () = msg_send![menu, addItem: &*item];
        // Register static items only — recents children are transient (menu-retained, per-open).
        if !in_recents && def.tag != 0 {
            refs_items.push((def.tag, Retained::as_ptr(&item) as *mut AnyObject));
            keep.push(item);
        }
    }
}

/// Re-apply titles + check state from the model, and the File menu's explicit enables. Called on
/// fingerprint change (tick) and on every menu open (menuNeedsUpdate).
unsafe fn apply_refresh(refs: &MenuRefs, snap: &MenuSnapshot) {
    let menus = build_menus(snap);
    let mut defs: HashMap<i32, (String, bool)> = HashMap::new();
    fn collect(items: &[ItemDef], out: &mut HashMap<i32, (String, bool)>) {
        for it in items {
            if !it.separator && it.tag != 0 && it.tag < TAG_RECENT_BASE && it.tag != TAG_RECENT_EMPTY {
                out.insert(it.tag, (it.title.clone(), it.checked));
            }
            if let Some(sub) = &it.submenu {
                collect(sub, out);
            }
        }
    }
    for (_, items) in &menus {
        collect(items, &mut defs);
    }
    for it in app_menu_inserts() {
        defs.insert(it.tag, (it.title.clone(), it.checked));
    }
    for (tag, ptr) in &refs.items {
        if ptr.is_null() {
            continue;
        }
        if let Some((title, checked)) = defs.get(tag) {
            let t = NSString::from_str(title);
            let () = msg_send![*ptr, setTitle: &*t];
            let () = msg_send![*ptr, setState: if *checked { 1isize } else { 0isize }];
        }
        if refs.explicit_tags.contains(tag) {
            let () = msg_send![*ptr, setEnabled: enabled_for(*tag, snap)];
        }
    }
}

/// Rebuild the Open Recent submenu from the snapshot (most-recent first, or the disabled
/// placeholder). Items are menu-retained; the old ones release on removal.
unsafe fn rebuild_recents(refs: &MenuRefs, snap: &MenuSnapshot) {
    let menu = refs.recent_menu;
    if menu.is_null() {
        return;
    }
    let n: isize = msg_send![menu, numberOfItems];
    for i in (0..n).rev() {
        let () = msg_send![menu, removeItemAtIndex: i];
    }
    let file = top_menus(snap)
        .into_iter()
        .find(|(id, _)| *id == TopMenu::File)
        .map(|(_, items)| items)
        .unwrap_or_default();
    let Some(parent) = file.into_iter().find(|i| i.tag == TAG_RECENT_PARENT) else { return };
    for def in parent.submenu.unwrap_or_default() {
        let item = new_action_item(&def, refs.controller);
        if def.tag == TAG_RECENT_EMPTY {
            // The recents menu autoenables (validate answers false too) — explicit for clarity.
            let () = msg_send![&*item, setEnabled: false];
        }
        let () = msg_send![menu, addItem: &*item];
    }
}

// ── install (the one-shot graft, polled by the tick until muda's bar exists) ─────────────────────

unsafe fn try_install() -> Option<MenuRefs> {
    // v0.9.61 (A2): the snapshot is READ HERE, from the one place that always holds a current one.
    // v0.9.60 gave the caller a `None` arm for the gated tick and a branch that reached back into
    // this mutex for it; the branch could then find `None` and silently do nothing, which is a state
    // to make impossible rather than to log. `tick_pump` stores the snapshot before anything can
    // reach this fn (the first tick's fingerprint is always a miss), so the `unwrap_or_default` is a
    // type-level formality, not a fallback with behaviour. Cloned out from under the lock before ANY
    // objc work, because `validateMenuItem:` takes this same mutex re-entrantly on this thread.
    let snap: MenuSnapshot = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    let snap = &snap;
    let Some(mtm) = MainThreadMarker::new() else {
        log_event("menubar: graft failed (not main thread)");
        return None;
    };
    let app = NSApplication::sharedApplication(mtm);
    let main_menu = live_main_menu(&app);
    if main_menu.is_null() {
        // muda installs its default bar at the FIRST window activation — poll (and say so once
        // if it never comes, e.g. a future backend change).
        bump_wait("no main menu from the backend");
        return None;
    }
    // ── v0.9.59 (S1 ruling 2): the POSITIVE FINGERPRINT GATE ──────────────────────────────────────
    // v0.9.22 gated the graft on `mainMenu != nil` and inferred "non-nil ⇒ after the first
    // activation", which is unsound in the other direction: nil is the state BEFORE muda builds the
    // bar, and appending into a bar that is not built yet is what produced "installed" over nothing.
    // So require the positive shape instead: item 0's submenu must carry a Quit (keyEquivalent "q").
    //
    // v0.9.61 (A2 — WHAT THIS GATE IS AND IS NOT): it is a "the bar is BUILT" test. It is not a
    // "the bar is MUDA'S" test and cannot be one — every standard macOS app menu carries ⌘Q, our own
    // re-grafted bar included. Discriminating the RIGHT bar is the ownership re-check's job
    // (pointer identity + the probe-item read-back), not this one's. Log ONCE what we actually saw
    // when it refuses — the line that settles the field's ordering-vs-recreation unknown.
    let app_item: *mut AnyObject = msg_send![main_menu, itemAtIndex: 0isize];
    let app_sub: *mut AnyObject =
        if app_item.is_null() { std::ptr::null_mut() } else { msg_send![app_item, submenu] };
    let rows = menu_rows(app_sub);
    let keys: Vec<String> = rows.iter().map(|(_, k)| k.clone()).collect();
    if !app_submenu_fingerprint_ok(&keys) {
        FINGERPRINT_LOGGED.with(|f| {
            if !f.get() {
                f.set(true);
                let shown: Vec<String> =
                    rows.iter().map(|(t, k)| if k.is_empty() { t.clone() } else { format!("{t}[{k}]") }).collect();
                log_event(&format!(
                    "menubar: graft target REFUSED — mainMenu {:p}, item0 \"{}\", app submenu {:p} [{}] carries no ⌘Q; \
                     this is not muda's bar, still polling",
                    main_menu,
                    item_string(app_item, true),
                    app_sub,
                    shown.join(", ")
                ));
            }
        });
        bump_wait("graft target never matched muda's bar");
        return None;
    }
    let Some(ctrl_ptr) = controller_ptr() else {
        log_event("menubar: graft failed (controller class)");
        return None;
    };

    let mut refs = MenuRefs {
        main_menu,
        probe_item: std::ptr::null_mut(),
        strong: Vec::with_capacity(3),
        items: Vec::with_capacity(64),
        explicit_tags: Vec::new(),
        recent_menu: std::ptr::null_mut(),
        controller: ctrl_ptr,
        last_fp: 0,
        keep: Vec::with_capacity(80),
    };
    if let Some(r) = Retained::retain(main_menu) {
        refs.strong.push(r); // the address `graft_state` compares must stay THIS object's address
    }

    // ── v0.9.61 (A2): SWEEP BEFORE APPEND ─────────────────────────────────────────────────────────
    // Clear AppKit's two role pointers FIRST — `setWindowsMenu:`/`setHelpMenu:` are being pointed at
    // menus this sweep is about to remove, and repeatedly re-registering role menus without ever
    // unregistering them is undocumented territory. Then remove every top-level holder we ever
    // added, in a loop. This is what makes a re-graft idempotent: v0.9.59's teardown dropped only
    // OUR refs, so an A→B→A bar replacement (or any bar that still carried our rows) got a second
    // full set of top-level menus appended to it, each with live, frozen duplicates dispatching into
    // a controller that outlives them all.
    let () = msg_send![&*app, setWindowsMenu: std::ptr::null_mut::<AnyObject>()];
    let () = msg_send![&*app, setHelpMenu: std::ptr::null_mut::<AnyObject>()];
    let swept = sweep_tag(main_menu, TAG_TOPLEVEL);

    // ── app submenu graft (muda's): Settings…/Welcome inserts + the Quit retarget ──
    // The fingerprint above guarantees app_sub is non-nil and holds a ⌘Q item.
    let mut quit = "retargeted";
    {
        // Quit: find muda's item by its ⌘Q equivalent and retarget it in place (title, position and
        // key equivalent stay; `terminate:` — which would bypass the post-run settings flush — goes).
        // v0.9.59 (S1 ruling 4): this runs on EVERY graft, so a re-grafted bar's ⌘Q is retargeted
        // again — the old bar's retarget dies with the old bar. ⌘Q then rides the queue → tick →
        // the IMMEDIATE-QUIT arm (OWNER RULING 08-02: "⌘Q always kills process"): no deferral, no
        // rotation reminder, no prompt — but still the graceful exit, because the loop RETURNS into
        // main()'s flush tail instead of dying inside `terminate:`. (⌘W keeps the title-bar ×
        // semantics.) L21/L26: `enabled_for(TAG_QUIT)` is unconditional, which is now the exact
        // mirror of a handler that can never refuse — pinned by
        // `a_scrim_bearing_modal_disables_the_two_panel_rows`.
        let n: isize = msg_send![app_sub, numberOfItems];
        let mut retargeted = false;
        for i in 0..n {
            let it: *mut AnyObject = msg_send![app_sub, itemAtIndex: i];
            if it.is_null() {
                continue;
            }
            let k: *mut AnyObject = msg_send![it, keyEquivalent];
            let is_q = !k.is_null() && (*(k as *const NSString)).to_string() == "q";
            if is_q {
                let () = msg_send![it, setTarget: ctrl_ptr];
                let () = msg_send![it, setAction: sel!(falconMenuAction:)];
                let () = msg_send![it, setTag: TAG_QUIT as isize];
                refs.items.push((TAG_QUIT, it));
                // v0.9.61 (A2): hold the retargeted Quit item strongly. `refs.items` carries it as a
                // bare pointer that `apply_refresh` messages every refresh, and it is muda's object,
                // not ours — the only thing keeping it alive is the submenu it sits in.
                if let Some(r) = Retained::retain(it) {
                    refs.strong.push(r);
                }
                retargeted = true;
                break;
            }
        }
        if !retargeted {
            quit = "MISSING";
            log_event("menubar: quit retarget FAILED — ⌘Q remains raw terminate: (flush-bypass risk)");
        }
        // v0.9.59 (S1 ruling 3): IDEMPOTENT inserts. A re-graft normally meets a FRESH muda submenu
        // (new bar, new objects), but if muda ever re-asserts the same submenu object our rows would
        // stack. Remove any previous copy of each tag — and of our own separator — first, so N
        // grafts leave exactly the same submenu as one.
        // v0.9.61 (A2): the per-tag removal is a LOOP now, like the separator sweep beside it. It
        // was a single `indexOfItemWithTag:` + one removal, which answers with the FIRST match only:
        // faced with the doubled submenu it exists to repair, it would have left the second copy of
        // every row standing and then inserted a third. One helper, both sweeps.
        for def in app_menu_inserts() {
            sweep_tag(app_sub, def.tag as isize);
        }
        sweep_tag(app_sub, TAG_APP_SEP);
        // Inserts after About + its separator (muda 0.19.3 layout; clamped defensively).
        let n: isize = msg_send![app_sub, numberOfItems];
        let mut idx = if n >= 2 { 2isize } else { n };
        for def in app_menu_inserts() {
            let item = new_action_item(&def, ctrl_ptr);
            let () = msg_send![app_sub, insertItem: &*item, atIndex: idx];
            refs.items.push((def.tag, Retained::as_ptr(&item) as *mut AnyObject));
            refs.keep.push(item);
            idx += 1;
        }
        let sep = new_separator();
        let () = msg_send![&*sep, setTag: TAG_APP_SEP];
        let () = msg_send![app_sub, insertItem: &*sep, atIndex: idx];
        refs.keep.push(sep);
    }

    // ── our menus: File · Edit · Photo · View · (Window) · Help ──
    // Language packs (round 2, PLAN §4): each menu is placed and configured by its id's role (a
    // Windows-tested decision in the model), and only the drawn title is translated.
    let model = top_menus(snap);
    for (id, items) in &model {
        let role = id.role();
        let title = id.title();
        if role.window_before {
            // Window sits between View and Help (macOS convention). Standard nil-target selectors —
            // the responder chain enables them and `setWindowsMenu:` lets AppKit populate the list.
            let window_title = i18n::tr("Window");
            let wmenu = new_menu(window_title);
            {
                let t = NSString::from_str(i18n::tr("Minimize"));
                let k = NSString::from_str("m");
                let alloc: Allocated<AnyObject> = msg_send_id![class!(NSMenuItem), alloc];
                let mi: Retained<AnyObject> =
                    msg_send_id![alloc, initWithTitle: &*t, action: sel!(performMiniaturize:), keyEquivalent: &*k];
                let () = msg_send![&*wmenu, addItem: &*mi];
                refs.keep.push(mi);
                let t = NSString::from_str(i18n::tr("Zoom"));
                let k = NSString::from_str("");
                let alloc: Allocated<AnyObject> = msg_send_id![class!(NSMenuItem), alloc];
                let zi: Retained<AnyObject> =
                    msg_send_id![alloc, initWithTitle: &*t, action: sel!(performZoom:), keyEquivalent: &*k];
                let () = msg_send![&*wmenu, addItem: &*zi];
                refs.keep.push(zi);
            }
            let holder = new_action_item(
                &ItemDef { tag: 0, title: window_title.into(), key: "", ctrl: false, checked: false, separator: false, submenu: None },
                ctrl_ptr,
            );
            // v0.9.61 (A2): the sweep tag — on THIS holder too. It is built in its own branch, above
            // the shared loop below, and it was the one top-level holder no teardown could reach.
            let () = msg_send![&*holder, setTag: TAG_TOPLEVEL];
            let () = msg_send![&*holder, setSubmenu: &*wmenu];
            let () = msg_send![main_menu, addItem: &*holder];
            let () = msg_send![&*app, setWindowsMenu: &*wmenu];
            refs.keep.push(wmenu);
            refs.keep.push(holder);
        }
        let menu = new_menu(title);
        let () = msg_send![&*menu, setDelegate: ctrl_ptr];
        if role.explicit_enables {
            // Explicit enablement here so a submenu PARENT honestly disables when its children are
            // all unavailable (AppKit auto-enables submenu parents regardless of validation):
            // File → Open Recent with no history, and — v0.8.119 (Y57) — View → Sort with no folder.
            let () = msg_send![&*menu, setAutoenablesItems: false];
        }
        let before = refs.items.len();
        realize_into(&menu, items, ctrl_ptr, &mut refs.items, &mut refs.keep, &mut refs.recent_menu, false);
        if role.explicit_enables {
            // EVERY realized tag in these menus (submenu children included — realize_into recurses
            // into `items`), because with autoenables off nothing else will ever set them.
            refs.explicit_tags.extend(refs.items[before..].iter().map(|(t, _)| *t));
        }
        let holder = new_action_item(
            &ItemDef { tag: 0, title: title.into(), key: "", ctrl: false, checked: false, separator: false, submenu: None },
            ctrl_ptr,
        );
        let () = msg_send![&*holder, setTag: TAG_TOPLEVEL]; // v0.9.61 (A2): swept by the next graft
        let () = msg_send![&*holder, setSubmenu: &*menu];
        let () = msg_send![main_menu, addItem: &*holder];
        if role.help_menu {
            let () = msg_send![&*app, setHelpMenu: &*menu];
        }
        refs.probe_item = Retained::as_ptr(&holder) as *mut AnyObject; // the read-back witness
        refs.keep.push(menu);
        refs.keep.push(holder);
    }
    // v0.9.61 (A2): the probe item's identity IS the Healthy arm's evidence (an `indexOfItem:` on
    // the live bar every tick), so hold it strongly — once, for the holder the loop settled on.
    if let Some(r) = Retained::retain(refs.probe_item) {
        refs.strong.push(r);
    }

    // ── v0.9.59 (S1 ruling 5 / L30): READ BACK before claiming success ────────────────────────────
    // "menubar: installed (7 menus; graft=ok)" was true about the CALLS it wrapped and false about
    // the menu bar on screen. A success line must assert the OS-VISIBLE outcome, so: re-read
    // NSApp.mainMenu (it can have been replaced DURING this graft) and ask AppKit whether the last
    // menu we appended is a child of THAT object. A miss returns None, so the next tick retries —
    // the inserts above are idempotent by construction.
    let live = live_main_menu(&app);
    let child_idx: isize =
        if live.is_null() { -1 } else { msg_send![live, indexOfItem: refs.probe_item] };
    if live != main_menu || child_idx < 0 {
        log_event(&format!(
            "menubar: graft did NOT take (grafted into {main_menu:p}, live mainMenu {live:p}, our Help holder index {child_idx}) — retrying"
        ));
        bump_wait("graft kept losing the read-back");
        return None;
    }
    refs.last_fp = fingerprint(snap);
    apply_refresh(&refs, snap);
    WAIT_TICKS.with(|w| w.set(0));
    let regrafts = REGRAFTS.with(|r| r.get());
    // v0.9.61 (A2 / L30): the line quotes the LIVE bar. v0.9.59 printed a hardcoded "7 menus" — a
    // literal that counted nothing and would have said "7" over the 13-menu duplicated bar this
    // wave exists to make impossible, which is the L30 defect inside the L30 fix. `numberOfItems`
    // and the top-level titles come from the object AppKit is showing, and the swept count says how
    // many of our own stale holders this graft had to remove to get there.
    let live_n: isize = msg_send![live, numberOfItems];
    log_event(&format!(
        "menubar: installed (live bar {live_n} top-level [{}]; swept {swept} stale holder(s); quit={quit}; {} tagged items; read-back OK — our menus ARE children of the live NSApp.mainMenu {main_menu:p}; re-grafts so far {regrafts})",
        toplevel_titles(live).join(", "),
        refs.items.len()
    ));
    Some(refs)
}

/// The poll counter + its one ~10 s diagnostic line, shared by every "not yet" exit above.
unsafe fn bump_wait(why: &str) {
    WAIT_TICKS.with(|w| {
        let t = w.get().saturating_add(1);
        w.set(t);
        if t == crate::menubar_model::GRAFT_POLL_FULL_TICKS {
            log_event(&format!("menubar: not installed after ~10 s ({why})"));
        }
    });
}

// ── the tick entry point ─────────────────────────────────────────────────────────────────────────

/// Called once per 16 ms tick on the UI thread: store the fresh snapshot (validation reads it),
/// install the bar once muda's exists, RE-CHECK that the bar we installed into is still the one
/// AppKit shows (v0.9.59 S1 ruling 1), re-apply titles/checks on state change, and return the
/// clicked tags for the caller's dispatch.
///
/// v0.9.60 (W2-1): `snap` is now OPTIONAL. `None` means the tick's cheap fingerprint
/// ([`menubar_model::tick_fingerprint`]) says nothing the menu renders from has moved since the
/// last push — so the stored snapshot is CURRENT, not stale, and there is nothing to store, hash,
/// or re-title. What still runs on a `None` tick is the part that has nothing to do with our state:
/// the mainMenu OWNERSHIP re-check (a couple of objc reads — the whole point of S1 ruling 1 is that
/// it happens every tick) and the command drain. A graft that becomes due on a `None` tick reads
/// the stored snapshot back out of the mutex, so the bar is still built from live state.
pub(crate) fn tick_pump(snap: Option<MenuSnapshot>) -> Vec<MenuCommand> {
    let fp = snap.as_ref().map(fingerprint);
    if let Some(s) = &snap {
        let mut g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(s.clone());
    }
    REFS.with(|r| {
        let mut b = r.borrow_mut();
        // The ownership re-check: one msg_send for `NSApp.mainMenu`, compared by POINTER against the
        // object this graft went into. muda re-asserts the SAME object on every activation, so a
        // healthy bar reads Healthy forever; a REPLACED bar (new activation target / window
        // re-creation) is the exact condition v0.9.22 could not see.
        let live = unsafe {
            MainThreadMarker::new()
                .map(|mtm| live_main_menu(&NSApplication::sharedApplication(mtm)) as usize)
                .unwrap_or(0)
        };
        let mut state = graft_state(b.as_ref().map(|refs| refs.main_menu as usize), live);
        // v0.9.61 (A2): pointer identity says the bar is the SAME OBJECT; it does not say our menus
        // are still IN it. A CONTENT probe — one `indexOfItem:` for the holder we kept as the
        // read-back witness — closes that gap, and it is O(7) over a menu bar's top level, on the
        // same tick that already does the pointer read. A miss drives a re-graft, which is only a
        // safe answer BECAUSE grafts are idempotent now (the TAG_TOPLEVEL sweep): before this wave
        // the same reaction would have duplicated the bar.
        if state == GraftState::Healthy {
            if let Some(refs) = b.as_ref() {
                let missing = unsafe {
                    let idx: isize = msg_send![refs.main_menu, indexOfItem: refs.probe_item];
                    idx < 0
                };
                if missing {
                    log_event("menubar: the bar is still our object but our menus are gone from it — re-grafting");
                    state = GraftState::Regraft;
                }
            }
        }
        // v0.9.61 (A2): never rebuild the bar out from under an OPEN menu. Tracking is a modal run
        // loop; a graft that becomes due during one is deferred to the next tick, which costs at
        // most a frame and is the difference between a documented sequence and an undocumented one.
        // The Healthy arm is unaffected — re-titling an item is not restructuring the bar.
        if state != GraftState::Healthy
            && live != 0
            && unsafe { menu_is_tracking(live as *mut AnyObject) }
        {
            state = GraftState::Healthy; // "not now" — the same condition is re-read next tick
        }
        match state {
            GraftState::Healthy => {
                // v0.9.60 (W2-1): a `None` tick carries no fingerprint BECAUSE the state did not
                // move, so there is nothing to re-title — the refresh is gated on a fingerprint
                // that CHANGED, exactly as before, and "absent" is not "changed".
                if let (Some(refs), Some(fp)) = (b.as_mut(), fp) {
                    if refs.last_fp != fp {
                        refs.last_fp = fp;
                        if let Some(s) = &snap {
                            unsafe { apply_refresh(refs, s) };
                        }
                    }
                }
            }
            GraftState::Install | GraftState::Regraft => {
                let regraft = state == GraftState::Regraft;
                if regraft {
                    // Tear down REFS first (ruling 1): the old menus are orphaned, and their strong
                    // refs go with them. The CONTROLLER is deliberately NOT part of this — the old
                    // bar's items still point at it (weakly) until AppKit releases them.
                    let old = b.take().map(|refs| refs.main_menu);
                    REGRAFTS.with(|c| c.set(c.get().saturating_add(1)));
                    // Detection line — deliberately NOT a success claim (L30): the "installed …
                    // read-back OK" line below is the only thing allowed to say the bar is live.
                    log_event(&format!(
                        "menubar: re-grafting (mainMenu changed: {:p} → {live:#x}) — the previous graft is orphaned",
                        old.unwrap_or(std::ptr::null_mut())
                    ));
                }
                // `should_attempt_graft` throttles ONLY the waiting case; a fresh Install/Regraft
                // request always tries immediately (WAIT_TICKS is reset by a successful graft).
                let waited = WAIT_TICKS.with(|w| w.get());
                if regraft || should_attempt_graft(waited) {
                    // v0.9.61 (A2): `try_install` reads the stored snapshot itself. v0.9.60 had a
                    // three-arm dance here (this tick's snapshot, else the stored one, else do
                    // nothing at all) whose last arm was an unreachable silent no-op — a state to
                    // make impossible, not to branch on. The stored snapshot is CURRENT by the
                    // gate's own contract (equal fingerprint ⇒ equal snapshot), so there is exactly
                    // one source and the caller no longer has to know about the mutex.
                    if let Some(refs) = unsafe { try_install() } {
                        *b = Some(refs);
                    }
                } else {
                    WAIT_TICKS.with(|w| w.set(w.get().saturating_add(1)));
                }
            }
        }
    });
    let mut q = CMD_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *q)
}
