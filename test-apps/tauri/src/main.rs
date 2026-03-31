// xa11y Tauri test app.
//
// Minimal Tauri 2 app that serves a static HTML frontend with the standard
// widget set for xa11y integration tests.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}
