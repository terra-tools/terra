/// Embeds the Windows icon resource into terra-app.exe.
///
/// Without this the built .exe has no icon of its own: Explorer, the Start
/// menu and the taskbar all show a blank default even though the running
/// window gets its icon from `app_icon()` at startup. Resources can only be
/// attached at link time, which is why this is a build script and not
/// something cargo-packager can bolt on afterwards.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon/terra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/terra.ico");
        res.set("ProductName", "Terra");
        res.set(
            "FileDescription",
            "Terra — a terminal your agents can drive",
        );
        res.compile()
            .expect("failed to embed the Windows icon resource");
    }
}
