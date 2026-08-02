//! The dictionary: stage 3½. Whisper-class models mishear the same words
//! the same way every time — "clod" for Claude, "say it" for sayit. Each
//! rule maps a misheard phrase to the exact text the user wanted, applied
//! to the transcript in the instant between transcription and paste.
//!
//! This module owns the whole feature: the rule type, the matcher, the
//! live in-memory rules, and the editor window's commands. settings.rs
//! only persists it; lib.rs only registers it.
//!
//! Hand-rolled like settings.rs: no regex crate for three string rules.
//! Matching is case-insensitive ("Clod", "clod", "CLOD" all hit one rule)
//! and word-boundary aware (a rule for "cat" must never fire inside
//! "category"). Cost is microseconds against the paste's own 60ms settle
//! delay — the edit step is free.

use crate::settings;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::Manager;

/// One dictionary rule: what the model mishears → what you actually said.
#[derive(Clone, Serialize, Deserialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

/// The live rules, held in memory so a take never touches the disk.
/// Loaded from settings.json at boot; replaced whole when the user edits.
#[derive(Default)]
pub struct Rules(pub Mutex<Vec<Replacement>>);

/// Boot wiring: saved rules into managed state, and the editor window's
/// close button turned into hide — the app lives in the tray; destroying
/// the webview would make reopening impossible.
pub fn init(app: &tauri::AppHandle, saved: &settings::Settings) {
    *app.state::<Rules>().0.lock().unwrap() = saved.replacements.clone();
    if let Some(w) = app.get_webview_window("dictionary") {
        let w2 = w.clone();
        w.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = w2.hide();
            }
        });
    }
}

/// Show + focus the editor window. Unlike the waveform, this window wants
/// focus — the user is here to type. Called from the tray and the
/// `dictionary_show` command.
pub fn show(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("dictionary") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The editor's read: current rules, for rendering the rows.
#[tauri::command]
pub fn dictionary_rules(rules: tauri::State<Rules>) -> Vec<Replacement> {
    rules.0.lock().unwrap().clone()
}

/// Save the whole rule list: live rules swap instantly (the very next
/// take uses them), then the settings file catches up on disk.
#[tauri::command]
pub fn dictionary_save(
    app: tauri::AppHandle,
    rules: tauri::State<Rules>,
    replacements: Vec<Replacement>,
) {
    *rules.0.lock().unwrap() = replacements.clone();
    let mut saved = settings::load(&app);
    saved.replacements = replacements;
    settings::save(&app, &saved);
}

/// Live preview for the editor's "try it" box. Runs the REAL apply() over
/// rules the user hasn't saved yet — one algorithm, no TS copy to drift.
#[tauri::command]
pub fn dictionary_preview(replacements: Vec<Replacement>, text: String) -> String {
    apply(&replacements, &text)
}

#[tauri::command]
pub fn dictionary_show(app: tauri::AppHandle) {
    show(&app);
}

/// Escape key in the editor. Hide, never destroy — same rule as the
/// CloseRequested handler in init().
#[tauri::command]
pub fn dictionary_hide(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("dictionary") {
        let _ = w.hide();
    }
}

/// Run every rule over the transcript, in order. Earlier rules win where
/// they overlap — "clod code" → "Claude Code" gets its chance before
/// "clod" → "Claude" — which is why this is a Vec, not a map.
pub fn apply(rules: &[Replacement], text: &str) -> String {
    let mut out = text.to_string();
    for rule in rules {
        if !rule.from.trim().is_empty() {
            out = apply_one(&out, &rule.from, &rule.to);
        }
    }
    out
}

/// A character that can be part of a word. Apostrophe included so
/// "it's" is one word and a rule for "it" doesn't fire inside it.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

/// Case-insensitive char equality. Comparing lowercase-expansions handles
/// "A"=="a" and friends; exotic one-to-many casings (ß) simply won't
/// cross-match, which is fine for dictated English.
fn ci_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn apply_one(text: &str, from: &str, to: &str) -> String {
    let pattern: Vec<char> = from.chars().collect();
    let chars: Vec<char> = text.chars().collect();
    // Only enforce a boundary on a side where the pattern edge is a word
    // character: "gpt-4" ends in '4' so "gpt-4s" must not match, but a
    // pattern ending in '.' abuts letters freely.
    let guard_start = pattern.first().copied().is_some_and(is_word);
    let guard_end = pattern.last().copied().is_some_and(is_word);

    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = i + pattern.len();
        let hit = end <= chars.len()
            && chars[i..end]
                .iter()
                .zip(&pattern)
                .all(|(&a, &b)| ci_eq(a, b))
            && !(guard_start && i > 0 && is_word(chars[i - 1]))
            && !(guard_end && end < chars.len() && is_word(chars[end]));
        if hit {
            out.push_str(to);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn replaces_ignoring_case() {
        let rules = [rule("clod", "Claude")];
        assert_eq!(apply(&rules, "ask Clod about it"), "ask Claude about it");
        assert_eq!(apply(&rules, "CLOD said no"), "Claude said no");
    }

    #[test]
    fn respects_word_boundaries() {
        let rules = [rule("cat", "Kat")];
        assert_eq!(apply(&rules, "the cat sat"), "the Kat sat");
        assert_eq!(apply(&rules, "a category error"), "a category error");
        assert_eq!(apply(&rules, "concat these"), "concat these");
    }

    #[test]
    fn multi_word_phrases() {
        let rules = [rule("say it", "sayit")];
        assert_eq!(apply(&rules, "I built say it today"), "I built sayit today");
    }

    #[test]
    fn earlier_rules_win() {
        let rules = [rule("clod code", "Claude Code"), rule("clod", "Claude")];
        assert_eq!(apply(&rules, "open clod code now"), "open Claude Code now");
    }

    #[test]
    fn punctuation_adjacent_still_matches() {
        let rules = [rule("clod", "Claude")];
        assert_eq!(apply(&rules, "thanks, clod."), "thanks, Claude.");
    }

    #[test]
    fn apostrophes_bind_words() {
        let rules = [rule("it", "IT")];
        assert_eq!(apply(&rules, "it's what it is"), "it's what IT is");
    }

    #[test]
    fn empty_from_is_ignored() {
        let rules = [rule("", "boom"), rule("  ", "boom")];
        assert_eq!(apply(&rules, "unchanged"), "unchanged");
    }

    #[test]
    fn replacement_at_string_edges() {
        let rules = [rule("clod", "Claude")];
        assert_eq!(apply(&rules, "clod"), "Claude");
        assert_eq!(
            apply(&rules, "clod at start and end clod"),
            "Claude at start and end Claude"
        );
    }
}
