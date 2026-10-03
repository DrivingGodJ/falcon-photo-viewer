// Built only for Windows targets on the Windows build host, using the same MSVC
// toolchain as the existing native dependencies. No runtime redistributable.
pub fn build() {
    use std::{hash::{Hash, Hasher}, path::PathBuf};
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") { return; }
    println!("cargo:rerun-if-changed=shell-icons/handler.cpp");
    println!("cargo:rerun-if-changed=build_shell_icons.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let compiler = cc::Build::new().cpp(true).static_crt(true).get_compiler();
    assert!(compiler.is_like_msvc(), "Falcon's Windows icon helper requires MSVC");
    for (stem, test_class) in [("falcon-shell-icons", false), ("falcon-shell-icons-test", true)] {
        let mut command = compiler.to_command();
        if test_class { command.arg("/DFALCON_ICON_TEST_CLASS"); }
        let status = command
            .args(["/nologo", "/LD", "/MT", "/O2", "/W4", "/WX", "/GS", "/guard:cf", "/GR-", "/std:c++17"])
            .arg(std::env::current_dir().unwrap().join("shell-icons/handler.cpp"))
            .arg(format!("/Fo{}", out.join(format!("{stem}.obj")).display()))
            .arg(format!("/Fe{}", out.join(format!("{stem}.dll")).display()))
            .args(["/link", "/INCREMENTAL:NO", "/DYNAMICBASE", "/NXCOMPAT", "/guard:cf", "/Brepro", "/EXPORT:DllGetClassObject", "/EXPORT:DllCanUnloadNow", "advapi32.lib", "shlwapi.lib", "uuid.lib"])
            .current_dir(&out).status().expect("compile Windows icon helper");
        assert!(status.success(), "Windows icon helper must build before packaging");
    }
    let bytes = std::fs::read(out.join("falcon-shell-icons.dll")).unwrap();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    // This is a cache name, not a trust digest. Runtime checks EVERY byte before
    // registration; a collision/mismatching file fails rather than being loaded.
    println!("cargo:rustc-env=FALCON_ICON_HELPER_NAME=falcon-icons-{:016x}.dll", hash.finish());
}
