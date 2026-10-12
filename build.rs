fn main() {
    // Icon, version info and the manifest (per-monitor DPI, common controls v6).
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
