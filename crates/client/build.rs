//! Embeds the executable icon (and version info from Cargo.toml) into the Windows build, so
//! endif.exe shows the rocket launcher in Explorer and the taskbar. The window icon at runtime is
//! set separately in `src/icon.rs`; other targets have no equivalent and skip this.
//!
//! Build scripts compile for the host, and `winresource` is only a build-dependency on Windows
//! hosts (Cargo.toml), so the body is compiled out elsewhere: the Linux runner builds the server
//! and the web client without it.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    // Only for Windows executables: the wasm build on a Windows host has nothing to embed.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=windows/endif.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("windows/endif.ico");
    res.set("ProductName", "endif.tf");
    res.set("FileDescription", "endif.tf");
    res.compile().expect("failed to compile the Windows resource file (is rc.exe available?)");
}
