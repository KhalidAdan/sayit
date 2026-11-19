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
//!
//! The same rules also work one step EARLIER, during decoding: their `to`
//! spellings become Whisper's prompt (`prompt`), so the engine is already
//! expecting "Tauri" when it hears it. `is_prompt_echo` catches the one
//! way that backfires — near-silence read back as the prompt itself.

use crate::settings;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

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
        // The page caches its rows from its last load; tell it to refetch
        // so a window shown hours after boot can never present a stale
        // list — which its autosave would then write back over the real one.
        let _ = w.emit("dictionary_reload", ());
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
    settings::update(&app, |saved| saved.replacements = replacements);
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

// ── The prompt: the dictionary, read to the engine before it listens ──
//
// whisper-server accepts a per-request `prompt`: text the decoder treats
// as if it had just been said, right before this audio. Words in it get
// more likely, spelled the way the prompt spells them — so "Tauri" comes
// out as "Tauri" instead of "Torai", before apply() ever has to fix it.
// apply() still runs afterwards for everything the prompt didn't fix.
//
// Whisper-only: a Parakeet-class engine has no prompt slot. If sayit ever
// switches, apply() keeps working and this simply stops being sent.

/// How long the prompt may get, in UTF-8 bytes.
///
/// Whisper's prompt slot holds 224 tokens (half its 448-token text
/// context). Overflow isn't an error: whisper.cpp keeps the LAST 224
/// tokens, silently cutting the FIRST terms — the ones listed first. So
/// the cap must fit a whole list with room to spare.
///
/// Measured with whisper.cpp's tokenizer over ggml-small's own vocabulary:
/// English names cost 2.2–2.6 bytes per token, but all-caps acronyms and
/// Japanese names only ~1.8 (rare text splits into small pieces). At hex's
/// 500, those lists overflow (~270 tokens); at 350 the densest measured
/// list is ~190 tokens. 350 bytes is still 35–45 English names.
///
/// Bytes, not characters, because the tokenizer eats bytes: for English
/// they're the same number; a Japanese character is 3 bytes and is
/// charged as 3 here, which is what keeps those lists inside the slot.
const PROMPT_BUDGET: usize = 350;

/// The prompt for this take, or None when the dictionary names nothing —
/// in which case the request carries no prompt field at all and is exactly
/// the request sayit always sent.
///
/// "Claude, sayit, Tauri": the vocabulary's terms joined with ", ".
pub fn prompt(vocabulary: &[String]) -> Option<String> {
    if vocabulary.is_empty() {
        None
    } else {
        Some(vocabulary.join(", "))
    }
}

/// The words the engine should expect this take: the distinct `to`
/// values, in rule order (like apply(), earlier rules matter more), as
/// many whole terms as fit PROMPT_BUDGET. Built once per take, under the
/// rules lock; prompt() sends it and is_prompt_echo() checks against it,
/// so the echo check knows exactly what was sent.
pub fn vocabulary(rules: &[Replacement]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut used = 0;
    for rule in rules {
        // One line, single-spaced: an expansion like "Best,\n  Khalid"
        // goes in as "Best, Khalid".
        let term = rule.to.split_whitespace().collect::<Vec<_>>().join(" ");
        let key = words(&term);
        // A deletion (empty `to`) or pure punctuation ("." for "full stop")
        // names nothing the engine could hear. A repeat — "Claude" from
        // both "clod" and "cloud", or "claude" after "Claude" — adds
        // nothing either; the first spelling wins, as in apply().
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        // Whole terms only, never a cut-off "Tau". A term too long to fit
        // is skipped, so one long expansion can't crowd out every name
        // after it; the shorter ones still get their turn.
        let separator = if terms.is_empty() { 0 } else { ", ".len() };
        let cost = separator + term.len();
        if used + cost > PROMPT_BUDGET {
            continue;
        }
        used += cost;
        seen.push(key);
        terms.push(term);
    }
    terms
}

/// Did the engine just read its prompt back to us?
///
/// On near-silence, Whisper sometimes "hears" the most likely text it has:
/// the prompt. Seen on the bundled server: quiet, unintelligible audio
/// (reversed speech at 5% volume), prompted with "Claude, sayit, Tauri,
/// Khalid, Parakeet, Effect-TS", came back as "Khalid, sayit, Tauri,
/// Khalid, Effect-TS". That's the engine talking, not you, so the caller
/// turns it into an empty take (the existing "heard nothing" path).
///
/// The rule: ignoring case, punctuation and spacing, the transcript is
/// nothing but two or more prompt terms back to back (repeats and any
/// order count — the echo above is both). Two things stay:
/// - exactly one listed name: saying just "Khalid" must still type it;
/// - real speech around the names: "ship sayit on Tauri" is a sentence.
///
/// The cost: a take that is deliberately just two listed names ("Claude
/// Tauri") is dropped too. That is the price of catching echoes that
/// aren't the whole prompt word for word.
pub fn is_prompt_echo(vocabulary: &[String], transcript: &str) -> bool {
    let heard = words(transcript);
    if heard.is_empty() {
        return false;
    }
    let terms: Vec<Vec<String>> = vocabulary.iter().map(|t| words(t)).collect();
    // Exactly one listed name is a real take.
    if terms.contains(&heard) {
        return false;
    }
    // Can the whole transcript be tiled, end to end, with terms?
    // can_reach[i] = "the first i heard words are a run of terms". Start
    // from 0 (nothing heard is trivially covered) and extend each reached
    // point by every term that matches there. Since the one-term case was
    // ruled out above, reaching the end means two or more terms.
    let mut can_reach = vec![false; heard.len() + 1];
    can_reach[0] = true;
    for i in 0..heard.len() {
        if !can_reach[i] {
            continue;
        }
        for term in &terms {
            if heard[i..].starts_with(term) {
                can_reach[i + term.len()] = true;
            }
        }
    }
    can_reach[heard.len()]
}

/// Lowercase words with punctuation and spacing dropped: "Effect-TS," →
/// ["effect", "ts"]. The one normalization both the term list and the echo
/// check use, so "same term" means one thing everywhere. Word characters
/// are is_word()'s, with apostrophes trimmed off the edges so a quoted
/// 'Claude' still reads as claude while "it's" stays one word.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !is_word(c))
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
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

    // ── prompt ──

    /// The six names the PR's TTS evidence used, as a dictionary would
    /// hold them.
    fn test_list() -> Vec<Replacement> {
        vec![
            rule("clod", "Claude"),
            rule("say it", "sayit"),
            rule("torai", "Tauri"),
            rule("hallid", "Khalid"),
            rule("periket", "Parakeet"),
            rule("effect ts", "Effect-TS"),
        ]
    }

    // The tests speak in rules, the way a take does: rules → vocabulary →
    // prompt / echo check.
    fn prompt_of(rules: &[Replacement]) -> Option<String> {
        prompt(&vocabulary(rules))
    }

    fn echo(rules: &[Replacement], transcript: &str) -> bool {
        is_prompt_echo(&vocabulary(rules), transcript)
    }

    #[test]
    fn no_rules_means_no_prompt() {
        // None → transcribe.rs sends no prompt field: today's request.
        assert_eq!(prompt_of(&[]), None);
    }

    #[test]
    fn prompt_is_the_to_spellings_in_rule_order() {
        assert_eq!(
            prompt_of(&test_list()).as_deref(),
            Some("Claude, sayit, Tauri, Khalid, Parakeet, Effect-TS")
        );
    }

    #[test]
    fn deletions_and_punctuation_contribute_nothing() {
        let rules = [rule("um", ""), rule("uh", "   "), rule("full stop", ".")];
        assert_eq!(prompt_of(&rules), None);
        let rules = [rule("um", ""), rule("clod", "Claude")];
        assert_eq!(prompt_of(&rules).as_deref(), Some("Claude"));
    }

    #[test]
    fn repeated_spellings_appear_once_first_spelling_wins() {
        let rules = [
            rule("clod", "Claude"),
            rule("cloud", "Claude"),
            rule("klod", "claude"),
            rule("tory", "Tauri"),
        ];
        assert_eq!(prompt_of(&rules).as_deref(), Some("Claude, Tauri"));
    }

    #[test]
    fn multi_line_expansions_become_one_line() {
        let rules = [rule("sig", "Best,\n  Khalid")];
        assert_eq!(prompt_of(&rules).as_deref(), Some("Best, Khalid"));
    }

    #[test]
    fn prompt_stops_at_a_whole_term_inside_the_budget() {
        // 40 distinct 9-byte terms (term00x01 …) = 40·9 + 39·2 = 438 bytes:
        // more than the budget holds.
        let rules: Vec<Replacement> = (0..40)
            .map(|i| rule("x", &format!("term{i:02}x{i:02}")))
            .collect();
        let p = prompt_of(&rules).unwrap();
        assert!(p.len() <= PROMPT_BUDGET, "{} bytes", p.len());
        // Every piece is a whole term, in order, with none cut short…
        let pieces: Vec<&str> = p.split(", ").collect();
        for (i, piece) in pieces.iter().enumerate() {
            assert_eq!(*piece, format!("term{i:02}x{i:02}"));
        }
        // …and the next term genuinely would not have fit.
        assert!(p.len() + ", ".len() + "term00x00".len() > PROMPT_BUDGET);
    }

    #[test]
    fn an_oversized_term_is_skipped_not_cut() {
        let long = "word ".repeat(100); // ~500 bytes: never fits
        let rules = [rule("sig", &long), rule("clod", "Claude")];
        assert_eq!(prompt_of(&rules).as_deref(), Some("Claude"));
    }

    #[test]
    fn budget_counts_bytes_not_characters() {
        // 田 is one character but three UTF-8 bytes, and the tokenizer is
        // charged per byte: 120 characters is 360 bytes — over budget,
        // though far under 350 characters. 100 of them (300 bytes) fit.
        assert_eq!(prompt_of(&[rule("x", &"田".repeat(120))]), None);
        assert!(prompt_of(&[rule("x", &"田".repeat(100))]).is_some());
    }

    // ── echo filter ──

    #[test]
    fn the_whole_prompt_read_back_is_an_echo() {
        let rules = test_list();
        // Case, punctuation and spacing don't matter.
        assert!(echo(
            &rules,
            "Claude, sayit, Tauri, Khalid, Parakeet, Effect-TS."
        ));
        assert!(echo(&rules, "claude sayit tauri khalid parakeet effect ts"));
    }

    #[test]
    fn a_partial_reordered_run_is_an_echo() {
        // Regression: exactly what the bundled whisper-server returned for
        // quiet unintelligible audio with test_list()'s prompt — not the
        // prompt word for word, so a whole-prompt match would miss it.
        assert!(echo(
            &test_list(),
            "Khalid, sayit, Tauri, Khalid, Effect-TS"
        ));
        assert!(echo(&test_list(), "Claude, sayit."));
    }

    #[test]
    fn exactly_one_listed_name_is_kept() {
        let rules = test_list();
        assert!(!echo(&rules, "Khalid."));
        assert!(!echo(&rules, "Effect-TS"));
        // Even when that name could also be read as two shorter terms.
        let rules = [
            rule("clod code", "Claude Code"),
            rule("clod", "Claude"),
            rule("kode", "Code"),
        ];
        assert!(!echo(&rules, "Claude Code."));
        assert!(echo(&rules, "Claude Code, Claude."));
    }

    #[test]
    fn one_term_prompt_said_twice_is_an_echo() {
        let rules = [rule("hallid", "Khalid")];
        assert!(!echo(&rules, "Khalid"));
        assert!(echo(&rules, "Khalid, Khalid."));
    }

    #[test]
    fn speech_around_the_names_is_kept() {
        let rules = test_list();
        // A real with-prompt transcript from the TTS run: wrong, but speech.
        assert!(!echo(&rules, "Khalid and Claude, sayit, Tauri"));
        assert!(!echo(&rules, "The app runs on Tauri with a Rust back-end."));
        // A term must match whole words, not a prefix of one.
        assert!(!echo(&rules, "Claudette, sayit"));
    }

    #[test]
    fn no_prompt_means_no_echo() {
        assert!(!echo(&[], "Claude, sayit"));
        // Deletion rules name nothing, so "um um" is not an echo of them.
        assert!(!echo(&[rule("um", "")], "um um"));
    }

    #[test]
    fn empty_transcript_is_not_an_echo() {
        // Already empty: nothing to drop (and nothing to log as an echo).
        assert!(!echo(&test_list(), ""));
        assert!(!echo(&test_list(), " . , "));
    }

    #[test]
    fn words_normalizes_case_punctuation_and_quotes() {
        assert_eq!(words("Effect-TS,"), ["effect", "ts"]);
        assert_eq!(words("  'Claude'  "), ["claude"]);
        assert_eq!(words("it's"), ["it's"]);
        assert!(words("... , !").is_empty());
    }
}
