//! Persistence for the key's few switches. A hand-rolled settings.json in
//! the app config dir — two fields don't need a plugin, and every line of
//! this is explainable. Load never fails (absent or corrupt file =
//! defaults); save never panics (a failed write costs one preference, not
//! a crash).

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// None = system default microphone.
    #[serde(default)]
    pub microphone: Option<String>,
    /// Pin the engine awake (skip the idle sleep timer).
    #[serde(default)]
    pub keep_awake: bool,
}

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, settings: &Settings) {
    let Some(p) = path(app) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(p, json);
    }
}
