//! Platform tray boundary. Windows uses Tauri's native tray implementation;
//! Linux publishes a StatusNotifierItem directly over D-Bus.

#[cfg(windows)]
#[path = "tray_windows.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "tray_linux.rs"]
mod platform;

pub use platform::Tray;

pub fn build(app: &tauri::AppHandle, saved: &crate::settings::Settings) -> Result<(), String> {
    platform::build(app, saved).map_err(|e| e.to_string())
}

pub fn set_status(app: &tauri::AppHandle, text: &str) {
    platform::set_status(app, text)
}
