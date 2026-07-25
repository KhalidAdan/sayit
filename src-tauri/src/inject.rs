//! Stage 4: the paste. Puts text on the clipboard, synthesizes Ctrl+V, and
//! restores what was there. This works because sayit never has focus — the
//! window is never shown, so the app the user was dictating into still owns
//! the cursor, and the paste lands there.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread::sleep;
use std::time::Duration;

pub fn inject(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;

    // Save what the user had. v1 preserves text only; an image on the
    // clipboard is lost to the paste. Known limitation, on the friction list.
    let saved = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .map_err(|e| e.to_string())?;
    // Windows clipboard updates are not instantaneous; pasting immediately
    // sometimes pastes the old contents. Small settle delay, found empirically.
    sleep(Duration::from_millis(60));

    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Unicode('v'), Direction::Click)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Release)
        .map_err(|e| e.to_string())?;

    // The receiving app reads the clipboard asynchronously; restoring too
    // soon hands it the old contents instead. 300ms is generous and invisible.
    sleep(Duration::from_millis(300));
    if let Some(saved) = saved {
        let _ = clipboard.set_text(saved);
    }
    Ok(())
}
