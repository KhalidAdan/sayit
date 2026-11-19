//! The Windows trigger: one F key, held to dictate. Which key is a choice
//! (settings.json `trigger_key`, switched live from the tray), so the claim
//! happens at runtime, not at build time. The plugin's build-time claim
//! treats "another app already owns this key" as fatal, which would turn a
//! bad pick into a sayit that can't launch. Here it costs a status line.
//!
//! "Claim" is Windows' RegisterHotKey underneath: one owner per key,
//! first come first served, and the key stops reaching every other app
//! while we hold it.

use crate::settings::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// The keys the tray offers. Bare F keys: every keyboard has them, and
/// there's no chord to keep held while talking.
pub(super) const KEYS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// The original key, kept as the default: almost nothing else uses F9.
const DEFAULT: &str = "F9";

/// Windows fires auto-repeat Pressed events while the key is held; only the
/// first may start a take.
static HELD: AtomicBool = AtomicBool::new(false);

/// Where the trigger stands. The tray's checkmark and the status-line hint
/// both read it.
enum Trigger {
    /// Before boot's claim; only visible in a startup race.
    Unclaimed,
    /// This key is ours; holding it dictates.
    Claimed(String),
    /// The chosen key belonged to another app at boot, so nothing is claimed.
    Busy(String),
}

static TRIGGER: Mutex<Trigger> = Mutex::new(Trigger::Unclaimed);

pub(super) fn on_shortcut(app: &AppHandle, state: ShortcutState) {
    match state {
        ShortcutState::Pressed => {
            if !HELD.swap(true, Ordering::SeqCst) {
                let _ = app.emit("push_started", crate::hotkey::now_ms());
            }
        }
        ShortcutState::Released => {
            if HELD.swap(false, Ordering::SeqCst) {
                let _ = app.emit("push_finished", crate::hotkey::now_ms());
            }
        }
    }
}

/// A settings value as one of our keys, or None. Forgiving about case and
/// stray spaces, since people edit settings.json by hand.
fn normalize(raw: &str) -> Option<String> {
    let key = raw.trim().to_ascii_uppercase();
    KEYS.contains(&key.as_str()).then_some(key)
}

/// The saved key, falling back to F9 (with a log line, not an error) when
/// the file holds something we don't offer.
pub(super) fn saved_key(saved: &Settings) -> String {
    normalize(&saved.trigger_key).unwrap_or_else(|| {
        eprintln!(
            "[sayit] trigger_key {:?} isn't F1–F12 — using {DEFAULT}",
            saved.trigger_key
        );
        DEFAULT.to_string()
    })
}

/// The key actually claimed right now, if any.
pub(super) fn claimed() -> Option<String> {
    match &*TRIGGER.lock().unwrap() {
        Trigger::Claimed(key) => Some(key.clone()),
        _ => None,
    }
}

/// The key the user chose, claimed or not. The tray checks this one.
pub(super) fn chosen() -> Option<String> {
    match &*TRIGGER.lock().unwrap() {
        Trigger::Claimed(key) | Trigger::Busy(key) => Some(key.clone()),
        Trigger::Unclaimed => None,
    }
}

pub(super) fn hint() -> String {
    hint_for(&TRIGGER.lock().unwrap())
}

fn hint_for(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Unclaimed => "no trigger key yet".into(),
        Trigger::Claimed(key) => format!("hold {key} to dictate"),
        Trigger::Busy(key) => format!("{key} is busy in another app; pick another key in the tray"),
    }
}

/// Boot: claim the saved key. A failure is recorded for the hint, not
/// raised; sayit still starts, and the tray can pick another key.
pub(super) fn claim_saved(app: &AppHandle) {
    let key = saved_key(&crate::settings::load(app));
    if let Err(why) = claim(app, &key) {
        eprintln!("[sayit] trigger: {why}");
        let mut trigger = TRIGGER.lock().unwrap();
        *trigger = Trigger::Busy(key);
        let _ = app.emit("trigger_changed", hint_for(&trigger));
    }
}

/// Make `key` the trigger. The new key is claimed before the old one is
/// released, so a failure leaves whatever worked still working.
pub(super) fn claim(app: &AppHandle, key: &str) -> Result<(), String> {
    let mut trigger = TRIGGER.lock().unwrap();
    if matches!(&*trigger, Trigger::Claimed(current) if current == key) {
        return Ok(());
    }
    let shortcuts = app.global_shortcut();
    shortcuts.register(key).map_err(|e| describe(key, e))?;
    if let Trigger::Claimed(old) = &*trigger {
        // Once the old key is released from us, its key-up can't reach us
        // either. Finish a take it started, or HELD would stay stuck and
        // the new key could never start one.
        if HELD.swap(false, Ordering::SeqCst) {
            let _ = app.emit("push_finished", crate::hotkey::now_ms());
        }
        if let Err(e) = shortcuts.unregister(old.as_str()) {
            eprintln!("[sayit] couldn't release {old}: {e}");
        }
    }
    *trigger = Trigger::Claimed(key.to_string());
    println!("[sayit] trigger key claimed: {key}");
    let _ = app.emit("trigger_changed", hint_for(&trigger));
    Ok(())
}

/// The plugin flattens global-hotkey's errors into strings; "already
/// registered" (Windows' ERROR_HOTKEY_ALREADY_REGISTERED) is the one a
/// person can act on, so it gets plain words.
fn describe(key: &str, error: tauri_plugin_global_shortcut::Error) -> String {
    let text = error.to_string();
    if text.contains("already registered") {
        format!("{key} is busy in another app")
    } else {
        format!("couldn't claim {key}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_keys_pass_through() {
        for key in KEYS {
            assert_eq!(normalize(key).as_deref(), Some(key));
        }
    }

    #[test]
    fn hand_edits_are_forgiven() {
        assert_eq!(normalize(" f7 ").as_deref(), Some("F7"));
    }

    #[test]
    fn keys_we_dont_offer_are_refused() {
        for raw in ["F13", "F0", "", "Ctrl+F9", "banana"] {
            assert_eq!(normalize(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_bad_saved_key_falls_back_to_f9() {
        let saved = Settings {
            trigger_key: "F13".into(),
            ..Settings::default()
        };
        assert_eq!(saved_key(&saved), "F9");
    }

    #[test]
    fn hints_name_the_key() {
        assert_eq!(
            hint_for(&Trigger::Claimed("F7".into())),
            "hold F7 to dictate"
        );
        assert!(hint_for(&Trigger::Busy("F7".into())).starts_with("F7 is busy"));
    }
}
