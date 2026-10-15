//! Persistence for the key's few switches. A hand-rolled settings.json in
//! the app config dir — a handful of fields don't need a plugin, and every
//! line of this is explainable. Load never fails (absent or corrupt file =
//! backup, then defaults); save never panics (a failed write costs one
//! preference, not a crash).
//!
//! The dictionary lives here too, which raises the stakes: losing this file
//! loses hours of accumulated rules. Three defenses, each against a real
//! way the file died in the wild:
//!
//! 1. **Atomic replace.** A plain `fs::write` truncates first — a crash,
//!    power cut, or antivirus lock mid-write leaves an empty file. We write
//!    a sibling tmp, fsync it, then rename over the original; the file on
//!    disk is always either the old version or the new one, never half.
//! 2. **Last-known-good backup.** Before each replace, the current file is
//!    copied to settings.json.bak — but only if it still parses, so a
//!    corrupt main can never shadow the good copy. Load falls back to it.
//! 3. **One writer at a time.** Every mutation goes through `update()`,
//!    which holds a process-wide lock across its read-modify-write. Without
//!    it, a tray toggle racing the dictionary's autosave could win the
//!    write with a stale rule list.
//!
//! A main file that exists but won't parse is stashed as
//! settings.json.corrupt instead of being silently replaced — hand-recovery
//! stays possible, and the fallback to defaults can never be mistaken for
//! the file's real contents.

use crate::dictionary::Replacement;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// Guards every read-modify-write (and the corrupt-stash rename inside
/// load). The file is small and writes are rare; contention is nil.
static LOCK: Mutex<()> = Mutex::new(());

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    let Some(p) = path(app) else {
        return Settings::default();
    };
    let _guard = LOCK.lock().unwrap();
    load_from(&p)
}

/// The one way to change settings: read, mutate, write — under the lock,
/// so no two writers can interleave and resurrect stale state.
pub fn update(app: &AppHandle, mutate: impl FnOnce(&mut Settings)) {
    let Some(p) = path(app) else { return };
    let _guard = LOCK.lock().unwrap();
    let mut settings = load_from(&p);
    mutate(&mut settings);
    save_to(&p, &settings);
}

/// Read a settings file and parse it, or say why not with None.
/// Hand-edited files (idle_minutes lives here) may carry a UTF-8 BOM,
/// which serde_json rejects — and a silent fall-back to defaults would
/// look like the edit was ignored.
fn read_valid(p: &Path) -> Option<Settings> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn bak(p: &Path) -> PathBuf {
    p.with_extension("json.bak")
}

fn load_from(p: &Path) -> Settings {
    if let Some(settings) = read_valid(p) {
        return settings;
    }
    if p.exists() {
        // The file is there but unreadable. Move it aside — if the next
        // save ran against defaults it would bury whatever is left in it,
        // and the dictionary deserves a hand-recovery chance.
        let stash = p.with_extension("json.corrupt");
        let _ = std::fs::rename(p, &stash);
        eprintln!(
            "[sayit] settings.json unreadable — stashed as {} and falling back to backup",
            stash.display()
        );
    }
    read_valid(&bak(p)).unwrap_or_default()
}

fn save_to(p: &Path, settings: &Settings) {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(settings) else {
        return;
    };
    // Keep a last-known-good copy — but only of a file that parses, so a
    // truncated main never overwrites the healthy backup.
    if read_valid(p).is_some() {
        let _ = std::fs::copy(p, bak(p));
    }
    // Atomic replace: sibling tmp (same volume, so rename can't degrade
    // to copy+delete), flushed to disk before the swap.
    let tmp = p.with_extension("json.tmp");
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()
    })();
    let swapped = written.and_then(|()| std::fs::rename(&tmp, p));
    if let Err(e) = swapped {
        eprintln!("[sayit] settings save failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory per test, so parallel tests never share a file.
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sayit-settings-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn with_rules(names: &[&str]) -> Settings {
        Settings {
            replacements: names
                .iter()
                .map(|n| Replacement {
                    from: n.to_string(),
                    to: n.to_uppercase(),
                })
                .collect(),
            ..Settings::default()
        }
    }

    #[test]
    fn roundtrip() {
        let p = dir("roundtrip").join("settings.json");
        save_to(&p, &with_rules(&["clod", "say it"]));
        let loaded = load_from(&p);
        assert_eq!(loaded.replacements.len(), 2);
        assert_eq!(loaded.replacements[0].from, "clod");
        assert_eq!(loaded.replacements[0].to, "CLOD");
    }

    #[test]
    fn save_keeps_a_backup_of_the_previous_version() {
        let p = dir("backup").join("settings.json");
        save_to(&p, &with_rules(&["first"]));
        save_to(&p, &with_rules(&["first", "second"]));
        let backup = read_valid(&bak(&p)).expect("backup exists and parses");
        assert_eq!(backup.replacements.len(), 1, "backup is the prior version");
        assert_eq!(load_from(&p).replacements.len(), 2);
    }

    #[test]
    fn corrupt_main_falls_back_to_backup_and_is_stashed() {
        let p = dir("corrupt-fallback").join("settings.json");
        save_to(&p, &with_rules(&["keeper"]));
        save_to(&p, &with_rules(&["keeper", "newer"]));
        // Simulate the truncation a crash mid-write used to leave behind.
        std::fs::write(&p, "{\"replacements\": [{\"fr").unwrap();

        let loaded = load_from(&p);
        assert_eq!(loaded.replacements.len(), 1, "backup rescued the rules");
        assert_eq!(loaded.replacements[0].from, "keeper");
        assert!(
            p.with_extension("json.corrupt").exists(),
            "the corrupt file is kept for hand recovery"
        );
        assert!(!p.exists(), "the corrupt file no longer shadows the backup");
    }

    #[test]
    fn corrupt_backup_is_never_refreshed_from_a_corrupt_main() {
        let p = dir("no-bad-backup").join("settings.json");
        save_to(&p, &with_rules(&["good"]));
        save_to(&p, &with_rules(&["good", "better"]));
        std::fs::write(&p, "garbage").unwrap();
        // A save over a corrupt main must not copy the garbage into .bak.
        save_to(&p, &with_rules(&["good", "better", "best"]));
        let backup = read_valid(&bak(&p)).expect("backup still parses");
        assert_eq!(backup.replacements[0].from, "good");
        assert_eq!(load_from(&p).replacements.len(), 3);
    }

    #[test]
    fn missing_everything_is_defaults() {
        let p = dir("missing").join("settings.json");
        let loaded = load_from(&p);
        assert!(loaded.replacements.is_empty());
        assert_eq!(loaded.idle_minutes, 5);
    }

    #[test]
    fn bom_is_tolerated() {
        let p = dir("bom").join("settings.json");
        std::fs::write(&p, "\u{feff}{\"idle_minutes\": 9}").unwrap();
        assert_eq!(load_from(&p).idle_minutes, 9);
    }
}
