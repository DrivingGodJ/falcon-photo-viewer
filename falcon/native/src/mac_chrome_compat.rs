//! Diagnostic, macOS-26-only compatibility variant. Public-host mode never enters here.
//! Informed by Chromium's BSD-licensed browser_native_widget_window_mac.mm and
//! override_ns_next_step_frame_hit_test.mm (the September 2026 title-bar investigation).
//! We subclass only identified Falcon-owned frame INSTANCES, without ivars; no global
//! NSNextStepFrame mutation, winit delegate replacement, system-band hiding or polling.
use objc2::declare::ClassBuilder;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{msg_send, sel, Encode};
use objc2_foundation::NSPoint;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(Clone)]
struct Context {
    window: Retained<AnyObject>,
    target: Option<Retained<AnyObject>>,
    active: bool,
    height: f64,
    inset: f64,
    host: usize,
}

struct Hook {
    frame: Retained<AnyObject>,
    original: &'static AnyClass,
    installed: &'static AnyClass,
    window: usize,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
    static HOOKS: RefCell<HashMap<usize, Hook>> = RefCell::new(HashMap::new());
    static PINNING: Cell<bool> = const { Cell::new(false) };
}

fn context() -> Option<Context> {
    CONTEXT.with(|c| c.try_borrow().ok().and_then(|c| c.clone()))
}

fn active_frame(this: *mut AnyObject) -> Option<Context> {
    let c = context().filter(|c| c.active)?;
    let own = HOOKS.with(|h| {
        h.try_borrow()
            .ok()
            .and_then(|h| {
                h.get(&(this as usize)).map(|h| {
                    h.window == &*c.window as *const AnyObject as usize || h.window == c.host
                })
            })
            .unwrap_or(false)
    });
    own.then_some(c)
}

unsafe fn original(this: *mut AnyObject) -> &'static AnyClass {
    // Entries precede class installation and outlive every installed callback.
    HOOKS
        .with(|h| h.borrow().get(&(this as usize)).map(|h| h.original))
        .unwrap_or_else(|| {
            (&*this)
                .class()
                .superclass()
                .expect("frame subclass has a superclass")
        })
}

unsafe fn call_double(this: *mut AnyObject, selector: Sel) -> f64 {
    let imp = original(this)
        .instance_method(selector)
        .expect("validated selector")
        .implementation();
    let f: unsafe extern "C" fn(*mut AnyObject, Sel) -> f64 = std::mem::transmute(imp);
    f(this, selector)
}

extern "C" fn height(this: *mut AnyObject, selector: Sel) -> f64 {
    unsafe { active_frame(this).map_or_else(|| call_double(this, selector), |c| c.height) }
}

extern "C" fn inset(this: *mut AnyObject, selector: Sel) -> f64 {
    unsafe {
        if let Some(c) = active_frame(this) {
            return c.inset; // Captured before hooks: querying buttons during layout can recurse.
        }
        call_double(this, selector)
    }
}

extern "C" fn centered(this: *mut AnyObject, selector: Sel) -> Bool {
    if active_frame(this).is_some() {
        return Bool::YES;
    }
    unsafe {
        let imp = original(this)
            .instance_method(selector)
            .expect("validated selector")
            .implementation();
        let f: unsafe extern "C" fn(*mut AnyObject, Sel) -> Bool = std::mem::transmute(imp);
        f(this, selector)
    }
}

fn pin_lights() {
    let Some(c) = context().filter(|c| c.active) else {
        return;
    };
    if PINNING.with(|p| p.replace(true)) {
        return;
    }
    unsafe {
        for kind in 0usize..3 {
            let button: *mut AnyObject = msg_send![&*c.window, standardWindowButton: kind];
            if !button.is_null() {
                let alpha: f64 = msg_send![button, alphaValue];
                if alpha != 1.0 {
                    let _: () = msg_send![button, setAlphaValue: 1.0f64];
                }
            }
        }
    }
    PINNING.with(|p| p.set(false));
}

extern "C" fn reveal(this: *mut AnyObject, selector: Sel, amount: f64) {
    unsafe {
        let imp = original(this)
            .instance_method(selector)
            .expect("validated selector")
            .implementation();
        let f: unsafe extern "C" fn(*mut AnyObject, Sel, f64) = std::mem::transmute(imp);
        f(this, selector, amount); // Preserve AppKit's layout, then correct button visibility.
    }
    if active_frame(this).is_some() {
        pin_lights();
    }
}

extern "C" fn hit_test(
    this: *mut AnyObject,
    selector: Sel,
    event: *mut AnyObject,
) -> *mut AnyObject {
    unsafe {
        if let Some(target) = active_frame(this).and_then(|c| c.target) {
            let event_type: usize = msg_send![event, type];
            let event_window: *mut AnyObject = msg_send![event, window];
            let frame_window: *mut AnyObject = msg_send![this, window];
            let target_window: *mut AnyObject = msg_send![&*target, window];
            if event_type == 3
                && !target_window.is_null()
                && event_window == target_window
                && frame_window == target_window
            {
                let p: NSPoint = msg_send![event, locationInWindow];
                let p: NSPoint =
                    msg_send![&*target, convertPoint: p fromView: std::ptr::null::<AnyObject>()];
                let bounds: objc2_foundation::NSRect = msg_send![&*target, bounds];
                if p.x >= bounds.origin.x
                    && p.y >= bounds.origin.y
                    && p.x < bounds.origin.x + bounds.size.width
                    && p.y < bounds.origin.y + bounds.size.height
                {
                    // WinitView processes the secondary click; never steal native button clicks.
                    return (&*target as *const AnyObject).cast_mut();
                }
            }
        }
        let imp = original(this)
            .instance_method(selector)
            .expect("validated selector")
            .implementation();
        let f: unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut AnyObject =
            std::mem::transmute(imp);
        f(this, selector, event)
    }
}

fn matches_method(class: &AnyClass, selector: Sel, returns: &str, args: &[&str]) -> bool {
    class.instance_method(selector).is_some_and(|m| {
        m.return_type().as_ref() == returns
            && m.arguments_count() == args.len() + 2
            && args
                .iter()
                .enumerate()
                .all(|(i, a)| m.argument_type(i + 2).is_some_and(|t| t.as_ref() == *a))
    })
}

pub(super) fn configure(
    window: Retained<AnyObject>,
    target: Option<Retained<AnyObject>>,
    height: f64,
) {
    let inset = unsafe {
        let button: *mut AnyObject = msg_send![&*window, standardWindowButton: 0usize];
        if button.is_null() {
            13.0
        } else {
            let frame: objc2_foundation::NSRect = msg_send![button, frame];
            20.0 - frame.size.width / 2.0
        }
    };
    let host = &*window as *const AnyObject as usize;
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            window,
            target,
            active: true,
            height,
            inset,
            host,
        })
    });
}

pub(super) fn set_active(active: bool) {
    CONTEXT.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            c.active = active;
        }
    });
    if active {
        pin_lights();
    }
}

/// Called from the owned accessory's lifecycle edge, so no geometry-based window guessing.
pub(super) unsafe fn attach(window: *mut AnyObject) {
    if window.is_null() || context().is_none() {
        return;
    }
    CONTEXT.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            c.host = window as usize;
        }
    });
    let content: *mut AnyObject = msg_send![window, contentView];
    if content.is_null() {
        return;
    }
    let frame: *mut AnyObject = msg_send![content, superview];
    install_frame(frame, window);
}

unsafe fn install_frame(frame: *mut AnyObject, window: *mut AnyObject) {
    if frame.is_null() || HOOKS.with(|h| h.borrow().contains_key(&(frame as usize))) {
        return;
    }
    let class = (&*frame).class();
    // No instance ivars, no mutation of a shared AppKit class. Use precisely matched ABIs.
    let h = matches_method(class, sel!(_titlebarHeight), "d", &[]);
    let i = matches_method(class, sel!(_minXTitlebarWidgetInset), "d", &[]);
    let b = matches_method(
        class,
        sel!(_shouldCenterTrafficLights),
        &Bool::ENCODING.to_string(),
        &[],
    );
    let r = matches_method(class, sel!(setButtonRevealAmount:), "v", &["d"]);
    let t = matches_method(class, sel!(_hitTestForEvent:), "@", &["@"]);
    super::record(format!("compat capabilities host={window:p} class={} height={h} inset={i} center={b} reveal={r} right_hit={t}", class.name()));
    if !(h || i || b || r || t) {
        return;
    }
    let name = format!(
        "FalconChrome02Frame_{:x}",
        class as *const AnyClass as usize
    );
    let installed = if let Some(c) = AnyClass::get(&name) {
        c
    } else {
        let Some(mut builder) = ClassBuilder::new(&name, class) else {
            return;
        };
        if h {
            builder.add_method(sel!(_titlebarHeight), height as extern "C" fn(_, _) -> _);
        }
        if i {
            builder.add_method(
                sel!(_minXTitlebarWidgetInset),
                inset as extern "C" fn(_, _) -> _,
            );
        }
        if b {
            builder.add_method(
                sel!(_shouldCenterTrafficLights),
                centered as extern "C" fn(_, _) -> _,
            );
        }
        if r {
            builder.add_method(
                sel!(setButtonRevealAmount:),
                reveal as extern "C" fn(_, _, _),
            );
        }
        if t {
            builder.add_method(
                sel!(_hitTestForEvent:),
                hit_test as extern "C" fn(_, _, _) -> _,
            );
        }
        builder.register()
    };
    if installed.instance_size() != class.instance_size() {
        return;
    }
    let Some(held) = Retained::retain(frame) else {
        return;
    };
    HOOKS.with(|hooks| {
        hooks.borrow_mut().insert(
            frame as usize,
            Hook {
                frame: held,
                original: class,
                installed,
                window: window as usize,
            },
        );
    });
    AnyObject::set_class(&*frame, installed);
    pin_lights();
}

fn restore_where(predicate: impl Fn(&Hook) -> bool) {
    // Keep callback lookup populated until native instances have their old classes back.
    let frames: Vec<_> = HOOKS.with(|h| {
        h.borrow()
            .values()
            .filter(|h| predicate(h))
            .map(|h| (h.frame.clone(), h.original, h.installed))
            .collect()
    });
    for (frame, original, installed) in frames {
        if std::ptr::eq(frame.class(), installed) {
            // objc2::set_class intentionally only admits subclasses, whereas restoration is
            // the inverse of our checked no-ivar change. Use the runtime primitive here.
            unsafe {
                extern "C" {
                    fn object_setClass(
                        object: *mut AnyObject,
                        class: *const AnyClass,
                    ) -> *const AnyClass;
                }
                object_setClass((&*frame as *const AnyObject).cast_mut(), original);
            }
            HOOKS.with(|h| {
                h.borrow_mut()
                    .remove(&(&*frame as *const AnyObject as usize));
            });
        } else {
            // Another instance-class owner intervened. Keep the original-method lookup alive
            // rather than clobbering its class or leaving our inherited callbacks dangling.
            super::record(
                "compat restore: frame class changed externally; retained forwarding metadata",
            );
        }
    }
}

pub(super) fn detach(window: *mut AnyObject) {
    if window.is_null() || context().is_some_and(|c| std::ptr::eq(&*c.window, window)) {
        return;
    }
    restore_where(|h| h.window == window as usize);
    CONTEXT.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            if c.host == window as usize {
                c.host = 0;
            }
        }
    });
}

pub(super) fn shutdown() {
    CONTEXT.with(|c| c.borrow_mut().take());
    restore_where(|_| true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::{class, msg_send_id};

    extern "C" fn original_height(_: *mut AnyObject, _: Sel) -> f64 {
        22.0
    }
    extern "C" fn wrong_height(_: *mut AnyObject, _: Sel) -> isize {
        22
    }

    // These use plain NSObject subclasses, not windows. The native CI gate can exercise
    // actual Objective-C dispatch/restoration without a visible GUI or a fullscreen Space.
    #[test]
    fn frame_hook_is_instance_scoped_forwards_when_inactive_and_restores() {
        unsafe {
            let mut builder =
                ClassBuilder::new("FalconChrome02TestFrame", class!(NSObject)).unwrap();
            builder.add_method(
                sel!(_titlebarHeight),
                original_height as extern "C" fn(_, _) -> _,
            );
            let class = builder.register();
            let frame: Retained<AnyObject> = msg_send_id![class, new];
            let peer: Retained<AnyObject> = msg_send_id![class, new];
            let ptr = (&*frame as *const AnyObject).cast_mut();
            install_frame(ptr, ptr); // No Context yet, so no native button queries.
            CONTEXT.with(|c| {
                *c.borrow_mut() = Some(Context {
                    window: frame.clone(),
                    target: None,
                    active: true,
                    height: 44.,
                    inset: 13.,
                    host: ptr as usize,
                })
            });
            let value: f64 = msg_send![&*frame, _titlebarHeight];
            let peer_value: f64 = msg_send![&*peer, _titlebarHeight];
            assert_eq!(value, 44.);
            assert_eq!(peer_value, 22.);
            set_active(false);
            let value: f64 = msg_send![&*frame, _titlebarHeight];
            assert_eq!(value, 22.);
            shutdown();
            assert!(std::ptr::eq(frame.class(), class));
            assert!(HOOKS.with(|h| h.borrow().is_empty()));
        }
    }

    #[test]
    fn unknown_selector_abi_is_not_overridden() {
        unsafe {
            let mut builder =
                ClassBuilder::new("FalconChrome02WrongAbi", class!(NSObject)).unwrap();
            builder.add_method(
                sel!(_titlebarHeight),
                wrong_height as extern "C" fn(_, _) -> _,
            );
            let class = builder.register();
            let frame: Retained<AnyObject> = msg_send_id![class, new];
            let ptr = (&*frame as *const AnyObject).cast_mut();
            install_frame(ptr, ptr);
            assert!(std::ptr::eq(frame.class(), class));
            let value: isize = msg_send![&*frame, _titlebarHeight];
            assert_eq!(value, 22);
            assert!(HOOKS.with(|h| h.borrow().is_empty()));
        }
    }
}
