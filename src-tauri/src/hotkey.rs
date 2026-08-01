//! Stage 1: the key. Collapses OS-level shortcut events into exactly two
//! logical signals — `push_started` and `push_finished` — and emits them
//! upward. Knows nothing about audio, models, or app state.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::ShortcutState;

/// The push-to-talk key. One line to change, per the north star.
pub const PUSH_TO_TALK: &str = "F9";

/// Whether the key is currently held. Windows fires auto-repeat `Pressed`
/// events for as long as a key is down; this flag collapses the burst into
/// one logical press. It also swallows nonsense transitions (a `Released`
/// with no preceding press).
static HELD: AtomicBool = AtomicBool::new(false);

/// Wall-clock ms, stamped at the OS event and carried in the payload so
/// the coordinator can measure how long the event took to cross into the
/// webview. Both sides read the same system clock, so the subtraction is
/// honest to within a millisecond.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn on_shortcut(app: &AppHandle, state: ShortcutState) {
    match state {
        ShortcutState::Pressed => {
            if !HELD.swap(true, Ordering::SeqCst) {
                let _ = app.emit("push_started", now_ms());
            }
        }
        ShortcutState::Released => {
            if HELD.swap(false, Ordering::SeqCst) {
                let _ = app.emit("push_finished", now_ms());
            }
        }
    }
}
