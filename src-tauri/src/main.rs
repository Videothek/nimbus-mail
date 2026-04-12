//! Nimbus — a modern mail client with Nextcloud integration.
//!
//! This is the Tauri application entry point. It initializes the
//! backend services and launches the native window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Nimbus");
}
