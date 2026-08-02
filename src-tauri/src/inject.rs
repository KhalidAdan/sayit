//! Stage 4: the paste. Puts text on the clipboard, synthesizes Ctrl+V, and
//! restores what was there. This works because sayit never has focus — the
//! window is never shown, so the app the user was dictating into still owns
//! the cursor, and the paste lands there.

#[cfg(windows)]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
#[cfg(windows)]
use std::thread::sleep;
#[cfg(windows)]
use std::time::{Duration, Instant};

/// Where the paste's milliseconds went. The key split is visible_ms vs
/// total_ms: the text is ON SCREEN at visible_ms — everything after is
/// clipboard-restore politeness the user never feels, but which the old
/// gap log silently counted as latency.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectTiming {
    /// Opening the clipboard + saving the user's old contents.
    pub clipboard_save_ms: u64,
    /// Putting our text on the clipboard.
    pub clipboard_set_ms: u64,
    /// The empirical wait for Windows to actually commit the clipboard.
    pub settle_ms: u64,
    /// Synthesizing Ctrl+V. The receiving app pastes as this lands.
    pub keystroke_ms: u64,
    /// Start of inject → text visible (the four phases above, summed).
    pub visible_ms: u64,
    /// The polite wait before restoring the old clipboard — the user's
    /// text is already on screen for all of it.
    pub restore_wait_ms: u64,
    pub total_ms: u64,
}

#[cfg(windows)]
pub fn inject(text: &str) -> Result<InjectTiming, String> {
    let t_all = Instant::now();

    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    // Save what the user had. v1 preserves text only; an image on the
    // clipboard is lost to the paste. Known limitation, on the friction list.
    let saved = clipboard.get_text().ok();
    let clipboard_save_ms = t_all.elapsed().as_millis() as u64;

    let t = Instant::now();
    clipboard
        .set_text(text.to_string())
        .map_err(|e| e.to_string())?;
    let clipboard_set_ms = t.elapsed().as_millis() as u64;

    // Windows clipboard updates are not instantaneous; pasting immediately
    // sometimes pastes the old contents. Small settle delay, found empirically.
    let t = Instant::now();
    sleep(Duration::from_millis(60));
    let settle_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
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
    let keystroke_ms = t.elapsed().as_millis() as u64;
    let visible_ms = t_all.elapsed().as_millis() as u64;

    // The receiving app reads the clipboard asynchronously; restoring too
    // soon hands it the old contents instead. 300ms is generous and invisible.
    let t = Instant::now();
    sleep(Duration::from_millis(300));
    if let Some(saved) = saved {
        // Restore on a detached thread: Windows clipboard access can block
        // indefinitely when another process (clipboard managers) holds it.
        // The paste already succeeded — a hung restore must cost at worst
        // the old clipboard contents, never the pipeline.
        std::thread::spawn(move || {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(saved);
            }
        });
    }
    let restore_wait_ms = t.elapsed().as_millis() as u64;

    let timing = InjectTiming {
        clipboard_save_ms,
        clipboard_set_ms,
        settle_ms,
        keystroke_ms,
        visible_ms,
        restore_wait_ms,
        total_ms: t_all.elapsed().as_millis() as u64,
    };
    println!(
        "[timing] inject: save {clipboard_save_ms}ms · set {clipboard_set_ms}ms · settle {settle_ms}ms · \
         keystroke {keystroke_ms}ms → visible at {visible_ms}ms · restore wait {restore_wait_ms}ms · total {}ms",
        timing.total_ms
    );
    Ok(timing)
}

#[cfg(target_os = "linux")]
pub fn inject(text: &str, input: &crate::linux_input::LinuxInput) -> Result<InjectTiming, String> {
    input.inject(text.to_owned())
}
