fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    resource
        .set_icon("../../assets/codex-cleaner.ico")
        .set("FileDescription", "Codex Cleaner")
        .set("ProductName", "Codex Cleaner")
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .set("InternalName", "CodexCleaner")
        .set("OriginalFilename", "CodexCleaner.exe");
    resource
        .compile()
        .expect("failed to embed Windows resources");
}
