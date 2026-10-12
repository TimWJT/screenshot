fn main() {
    // Icon, version info and the manifest (per-monitor DPI, common controls v6).
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
    // Load Windows DLLs only from System32, never from the exe's folder, so a DLL dropped next
    // to the portable exe (in Downloads, say) can't be loaded in their place.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
