//! The Slint UI compilation unit — a `slint::slint!` macro-as-stub (v0.9.6, the
//! ui.rs extraction series complete): every component/global/struct lives in an
//! external .slint file under native/ui/ — MainWindow included (main_window.slint).
//! The macro imports and re-exports exactly the symbols main.rs's generated API
//! needs, so the generated types reach the crate root via `use ui::*` unchanged.

slint::slint! {
    // ONE compilation unit still: main_window.slint pulls in the component files, and
    // the macro emits include_bytes! markers per imported file, so .slint edits
    // retrigger cargo. Zero definitions remain here — this is a pure re-export stub.
    import { ToolbarState } from "../ui/toolbar.slint";
    import { MacToolbarWindow } from "../ui/mac_toolbar.slint";
    import { MainWindow } from "../ui/main_window.slint";
    import { Theme, Tip } from "../ui/theme.slint";
    import { FilmItem, SelTileRow, ExifRow, ExifBrief, KeybindRow, NotifRow, CopyPrefRow, WmFontRow, DisplaySection } from "../ui/structs.slint";
    export { MainWindow, MacToolbarWindow, ToolbarState, Theme, Tip, FilmItem, SelTileRow, ExifRow, ExifBrief, KeybindRow, NotifRow, CopyPrefRow, WmFontRow, DisplaySection }
}
