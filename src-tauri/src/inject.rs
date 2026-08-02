//! Stage 4: the paste — the contract half. The actual injection lives in
//! each platform backend (`platform::Platform::inject`); what stays here is
//! the timing breakdown every backend must account for. This works because
//! sayit never has focus — the window is never shown, so the app the user
//! was dictating into still owns the cursor, and the paste lands there.

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
    /// The empirical wait for the OS to actually commit the clipboard.
    pub settle_ms: u64,
    /// Synthesizing the paste chord. The receiving app pastes as this lands.
    pub keystroke_ms: u64,
    /// Start of inject → text visible (the four phases above, summed).
    pub visible_ms: u64,
    /// The polite wait before restoring the old clipboard — the user's
    /// text is already on screen for all of it.
    pub restore_wait_ms: u64,
    pub total_ms: u64,
}
