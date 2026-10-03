//! Explicit opt-in renderer capture for menu QA. No OS input or desktop capture.
//! The normal menu effect never reads the GPU back; only this explicit test hook does.
use crate::*;

fn capture_directory() -> Option<PathBuf> {
    std::env::var_os("FALCON_DEBUG_MENU_CAPTURE").filter(|p| !p.is_empty())
        .map(PathBuf::from).filter(|p| p.is_absolute())
}

pub(crate) fn legacy() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        capture_directory().is_some()
            && std::env::var_os("FALCON_DEBUG_MENU_LEGACY").is_some_and(|v| v == "1")
    })
}

pub(crate) fn arm(app: &MainWindow) {
    let Some(dir) = capture_directory() else {
        return;
    };
    let kind = std::env::var("FALCON_DEBUG_MENU_KIND").unwrap_or_default();
    let sort = kind == "sort";
    let about = kind == "about";
    let settings = kind == "settings";
    let speed = kind == "speed";
    let review=std::env::var_os("FALCON_DEBUG_MENU_REVIEW").is_some();
    if review {
        let aw=app.as_weak();
        slint::Timer::single_shot(Duration::from_secs(3),move || {
            if let Some(a)=aw.upgrade() {a.set_sel_open(true);}
        });
    }
    let aw = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(5), move || {
        let Some(a) = aw.upgrade() else {
            return;
        };
        if a.get_count_all() < 4 || !a.get_photo_ready() {
            log_event("menu-probe: scene not ready");
            return;
        }
        if about { a.set_settings_open(true); a.invoke_show_about(); return; }
        if settings { a.set_settings_open(true); return; }
        if speed { a.set_loading_open(true); return; }
        if sort {
            a.invoke_toolbar_action("sort".into(), 240., a.get_content_top());
            return;
        }
        a.set_ctx_target(3);
        a.invoke_ctx_populate(3);
        if review {
            a.set_sel_ctx_idx(3);a.set_sel_ctx_x(40.);a.set_sel_ctx_y(104.);a.set_sel_ctx_open(true);
        } else {a.set_ctx_x(40.);a.set_ctx_y(60.);a.set_ctx_open(true);}
    });
    if sort {
        let aw=app.as_weak();
        slint::Timer::single_shot(Duration::from_secs(6), move || {
            if let Some(a)=aw.upgrade() {
                a.window().dispatch_event(slint::platform::WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(a.get_context_menu_x()+24.,
                        a.get_content_top()+a.get_context_menu_y()+65.),
                });
            }
        });
    }
    let aw = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(8), move || {
        let Some(a) = aw.upgrade() else {
            return;
        };
        if !a.get_ctx_open() && !a.get_sel_ctx_open() && !a.get_sort_open()
            && !a.get_settings_open() && !a.get_loading_open() && !a.get_about_open() {
            log_event("menu-probe: no menu to capture");
            return;
        }
        log_event(&format!(
            "menu-probe: region ready={} panel={},{},{},{} legacy={} scale={}",
            a.get_menu_blur_ready(),
            a.get_context_menu_x(),
            a.get_context_menu_y(),
            a.get_context_menu_w(),
            a.get_context_menu_h(),
            legacy(),
            a.window().scale_factor()
        ));
        match a.window().take_snapshot() {
            Ok(pixels) => {
                std::thread::spawn(move || {
                    let result = (|| -> Result<(), Box<dyn Error>> {
                        std::fs::create_dir_all(&dir)?;
                        use std::io::Write;
                        let mut file = std::io::BufWriter::new(
                            std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(dir.join("menu.ppm"))?,
                        );
                        write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                        let rgb: Vec<u8> = pixels
                            .as_bytes()
                            .chunks_exact(4)
                            .flat_map(|p| p[..3].iter().copied())
                            .collect();
                        file.write_all(&rgb)?;
                        file.flush()?;
                        Ok(())
                    })();
                    match result {
                        Ok(()) => log_event("menu-probe: captured renderer output"),
                        Err(e) => log_event(&format!("menu-probe: capture write failed: {e}")),
                    }
                });
            }
            Err(e) => log_event(&format!("menu-probe: renderer capture failed: {e}")),
        }
    });
    slint::Timer::single_shot(Duration::from_secs(12), || {
        let _ = slint::quit_event_loop();
    });
}
