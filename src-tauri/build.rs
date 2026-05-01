fn main() {
    // Tauri embeds the .exe / .app icon as a Windows resource (or
    // macOS bundle asset) at build time. When only the icon bytes
    // change — e.g. running `cargo tauri icon` after a logo
    // refresh — Cargo's incremental compilation has no Rust-source
    // signal to rerun this script, so the old icon stays baked into
    // the cached binary and surfaces in Task Manager / Finder until
    // the next clean rebuild. Adding explicit `rerun-if-changed`
    // hints fixes that — Cargo will re-run `tauri_build::build()`
    // (which re-embeds the resource) the moment any icon file's
    // mtime changes.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");

    tauri_build::build();
}
