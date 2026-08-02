//! Persistence for the key's few switches. A hand-rolled settings.json in
//! the app config dir — a handful of fields don't need a plugin, and every
//! line of this is explainable. Load never fails (absent or corrupt file =
//! defaults); save never panics (a failed write costs one preference, not
//! a crash).

use crate::dictionary::Replacement;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    /// None = system default microphone.
    #[serde(default)]
    pub microphone: Option<String>,
    /// Pin the engine awake (skip the idle sleep timer).
    #[serde(default)]
    pub keep_awake: bool,
    /// Minutes of idle before the engine sleeps and its VRAM comes home.
    /// Edit settings.json to change it; 0 disables auto-sleep entirely
    /// (same effect as keep_awake). Kept small on purpose — an idle engine
    /// squatting VRAM overnight starves every other GPU workload on the
    /// machine, and a wake costs only seconds.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u64,
    /// The dictionary: applied in order to every transcript, before the paste.
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    /// First-run bootstrap completed at least once. Only meaningful on
    /// platforms whose backend says needs_setup(); the rest ignore it, so
    /// existing installs never acquire onboarding. The alias keeps
    /// settings.json files written before the rename readable.
    #[serde(default, alias = "linux_setup_complete")]
    pub setup_complete: bool,
}

fn default_idle_minutes() -> u64 {
    5
}

/// Hand-rolled (not derived) so a missing settings.json gets the same
/// idle default as a settings.json missing the field.
impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: None,
            keep_awake: false,
            idle_minutes: default_idle_minutes(),
            replacements: Vec::new(),
            setup_complete: false,
        }
    }
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
        // Hand-edited files (idle_minutes lives here) may carry a UTF-8
        // BOM, which serde_json rejects — and a silent fall-back to
        // defaults would look like the edit was ignored.
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
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
