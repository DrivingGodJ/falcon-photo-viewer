//! Packaged shell icons. Extension routing does not open or classify any photo.
#[allow(dead_code)]
#[cfg(any(windows, test))]
mod catalog {
    include!("../assets/icons/catalog.rs");
}

#[cfg(windows)]
pub(crate) const LEGACY_PROGID: &str = "Falcon.Image";
#[cfg(any(windows, test))]
pub(crate) const GENERIC_DOCUMENT_ID: u16 = 2;

#[cfg(windows)]
pub(crate) fn progid(ext: &str) -> String {
    format!("{LEGACY_PROGID}.{ext}")
}

#[cfg(any(windows, test))]
pub(crate) fn resource(ext: &str, distinct_raw: bool) -> u16 {
    catalog::EXTENSIONS.iter().find(|(e, _, _)| *e == ext)
        .map(|(_, off, on)| if distinct_raw { *on } else { *off })
        .unwrap_or(GENERIC_DOCUMENT_ID)
}

#[cfg(windows)]
pub(crate) fn reference(exe: &str, id: u16) -> String {
    // Negative means resource ID, not position in the executable's icon table.
    format!("\"{exe}\",-{id}")
}

pub(crate) fn wire_preference(app: &crate::MainWindow, update: impl Fn(bool) -> std::io::Result<()> + 'static) {
    use slint::ComponentHandle;
    let weak = app.as_weak();
    app.on_distinct_raw_icons_toggled(move || {
        if let Some(a) = weak.upgrade() {
            let next = !a.get_distinct_raw_icons();
            match update(next) {
                Ok(()) => { a.set_distinct_raw_icons(next); a.set_assoc_status("".into()); }
                Err(e) => {
                    let restore = update(!next);
                    let note = restore.err().map(|e| format!(" Couldn't restore the previous icons: {e}")).unwrap_or_default();
                    a.set_assoc_status(slint::format!("Couldn't update the RAW icons: {e}{note}"));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Falsifier: omit a supported extension, share PNG/APNG, or route WebP to motion.
    #[test]
    fn every_supported_extension_has_the_approved_resource() {
        let expected: std::collections::BTreeSet<_> = crate::support::ASSOC_FAMILIES.iter()
            .flat_map(|(_, e)| e.iter().copied()).collect();
        let actual: std::collections::BTreeSet<_> = catalog::EXTENSIONS.iter().map(|v| v.0).collect();
        assert_eq!(expected, actual);
        assert_eq!(actual.len(), catalog::EXTENSIONS.len());
        let names: std::collections::HashMap<_, _> = catalog::RESOURCES.iter().copied().collect();
        for ext in falcon_decode::RAW_EXTS {
            assert_eq!(names[&resource(ext, false)], "raw.ico");
            assert_eq!(names[&resource(ext, true)], format!("{ext}.ico"));
        }
        for (ext, name) in [("jpg", "jpg"), ("jpeg", "jpg"), ("png", "png"),
            ("apng", "apng"), ("tif", "tiff"), ("tiff", "tiff"), ("heic", "heic"),
            ("heif", "heif"), ("webp", "webp"), ("gif", "gif"), ("bmp", "bmp"), ("jxl", "jxl")] {
            assert_eq!(names[&resource(ext, false)], format!("{name}.ico"));
            assert_eq!(resource(ext, false), resource(ext, true));
        }
        assert_eq!(resource("unknown", true), GENERIC_DOCUMENT_ID);
        assert_eq!(catalog::RESOURCES[0], (1, "app.ico"));
    }
}
