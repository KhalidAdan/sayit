//! The Windows paste, and the mess it no longer leaves. The shape is v1's —
//! save the clipboard, put the transcript on it, settle, Ctrl+V, put the
//! old clipboard back — with four courtesies learned from hex's Windows
//! paste (its src/windows/paste.rs, MIT; studied, not copied):
//!
//! 1. **Out of the history.** The transcript goes on the clipboard marked
//!    "don't keep this", so a dictation never shows up in Win+V, never
//!    syncs to the cloud clipboard, and is skipped by clipboard managers
//!    that honour the mark. The clipboard is the delivery van; the van
//!    keeps no log.
//! 2. **Restore only what's still ours.** Windows numbers every clipboard
//!    write. We note the number right after our paste goes on; if it has
//!    moved by restore time, someone (you) copied something since, and
//!    that copy wins. v1 restored blindly and could eat it.
//! 3. **Hands off first.** A Shift, Alt or Win the user is still holding
//!    turns our Ctrl+V into a different shortcut. We wait for them to come
//!    up — overlapped with the settle we pay anyway, so it costs time only
//!    while one is physically down, and never more than a second.
//! 4. **A real Ctrl+V.** Ctrl and V as virtual keys, each with the scan
//!    code the foreground window's keyboard layout gives it, the way a
//!    physical keyboard sends them — in ONE SendInput call, leaving a Ctrl
//!    the user holds alone. v1 used enigo's `Key::Unicode('v')`, which asks
//!    the layout which key types a "v" (VkKeyScanExW); a Russian, Greek,
//!    Arabic or Hebrew layout has none, and enigo then falls back to typing
//!    the letter as a Unicode packet — Ctrl plus a typed "v" is not a
//!    paste. VK_V is the paste key on every layout. enigo also sent the
//!    chord as three separate SendInput calls, and pressed and released
//!    Ctrl even when the user was holding it.

use crate::inject::InjectTiming;
use arboard::SetExtWindows;
use std::mem::size_of;
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyExW, SendInput, HKL, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_CONTROL, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Windows clipboard updates are not instantaneous; pasting immediately
/// sometimes pastes the old contents. Small settle delay, found
/// empirically in v1 and paid on every paste.
const SETTLE: Duration = Duration::from_millis(60);

/// The longest the paste waits for a held modifier (settle included)
/// before it goes anyway. A second is long enough for a finger that was
/// on its way up, and short enough that a stuck key can't hang the take.
const HOLD_LIMIT: Duration = Duration::from_secs(1);

/// How often we look at the keyboard while a modifier is down.
const POLL: Duration = Duration::from_millis(10);

/// The receiving app reads the clipboard asynchronously; restoring too
/// soon hands it the old contents instead. 300ms is generous and invisible.
const RESTORE_WAIT: Duration = Duration::from_millis(300);

/// Keys that bend Ctrl+V into another shortcut while held: Ctrl+Shift+V
/// (paste as plain text; Markdown preview in VS Code), Ctrl+Alt+V (Paste
/// Special in Office; and Ctrl+Alt is AltGr, which types characters on
/// many layouts), Win+Ctrl+V (Windows 11's sound-output picker). Ctrl is
/// not here: a held Ctrl is already half of the chord. The two Win keys
/// share one name so the log says "Win" once.
const CONFLICTING_MODIFIERS: [(u16, &str); 4] = [
    (VK_SHIFT, "Shift"),
    (VK_MENU, "Alt"),
    (VK_LWIN, "Win"),
    (VK_RWIN, "Win"),
];

/// The blocking paste (see `Platform::inject`). The same five beats as v1,
/// timed the same way, so `InjectTiming` means what it always meant.
pub(super) fn inject(text: &str) -> Result<InjectTiming, String> {
    let t_all = Instant::now();

    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    // Save what the user had. v1 preserves text only; an image on the
    // clipboard is lost to the paste. Known limitation, on the friction list.
    let saved = clipboard.get_text().ok();
    let clipboard_save_ms = t_all.elapsed().as_millis() as u64;

    let t = Instant::now();
    unlisted(clipboard.set())
        .text(text)
        .map_err(|e| e.to_string())?;
    // Our paste's place in the clipboard's history. The restore compares
    // against it to tell "still ours" from "the user copied since".
    let pasted = sequence_number();
    let clipboard_set_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let hands = wait_to_paste(key_down, || t.elapsed(), sleep);
    let settle_ms = t.elapsed().as_millis() as u64;
    match hands {
        Hands::Free => {}
        Hands::LetGo(keys) => println!(
            "[paste] waited for {} to come up: Ctrl+V at {settle_ms}ms instead of {}ms",
            keys.join("+"),
            SETTLE.as_millis()
        ),
        Hands::StillHeld(keys) => eprintln!(
            "[paste] {} still held after {settle_ms}ms; pasting anyway, the app may see another shortcut",
            keys.join("+")
        ),
    }

    let t = Instant::now();
    // A failed keystroke returns here, BEFORE the restore: the transcript
    // stays on the clipboard so a manual Ctrl+V can still land it. The
    // dictation is worth more than the old clipboard (v1 did the same).
    press_ctrl_v()?;
    let keystroke_ms = t.elapsed().as_millis() as u64;
    let visible_ms = t_all.elapsed().as_millis() as u64;

    let t = Instant::now();
    sleep(RESTORE_WAIT);
    if let Some(saved) = saved {
        // Restore on a detached thread: Windows clipboard access can block
        // indefinitely when another process (clipboard managers) holds it.
        // The paste already succeeded — a hung restore must cost at worst
        // the old clipboard contents, never the pipeline.
        std::thread::spawn(move || restore(saved, pasted));
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

// ───────── 1. out of the history ─────────

/// Mark a clipboard write "don't keep this". Microsoft documents three
/// registered clipboard formats for it; arboard (3.6) writes each one in
/// the same open-clipboard session as the text, so no listener ever sees
/// the text without its marks:
///
/// - `ExcludeClipboardContentFromMonitorProcessing`: the broad one. Per
///   Microsoft it keeps the item out of both history and cloud sync, and
///   it's the convention third-party clipboard managers look for (some
///   keep their history in a database on disk).
/// - `CanIncludeInClipboardHistory` = 0: Windows' own Win+V history.
/// - `CanUploadToCloudClipboard` = 0: "sync across your devices", which
///   means Microsoft's servers.
///
/// arboard's docs advise that the first one makes the other two
/// unnecessary. We set all three anyway: it costs three 4-byte writes and
/// doesn't bet on every listener reading the same mark.
fn unlisted(set: arboard::Set<'_>) -> arboard::Set<'_> {
    set.exclude_from_monitoring()
        .exclude_from_history()
        .exclude_from_cloud()
}

// ───────── 2. restore only what's still ours ─────────

/// Windows' running count of clipboard writes on this desktop — anyone's
/// write bumps it. Two equal readings mean nobody wrote in between.
fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

#[derive(Debug, PartialEq)]
enum Restore {
    /// The clipboard still holds our paste: put the user's text back.
    PutBack,
    /// The clipboard moved on since our paste — the user (or some app)
    /// copied something. That copy is theirs; leave it.
    LeaveIt,
}

/// The restore decision, pure so it can be tested. Writing the clipboard
/// bumps the number; an app pasting only reads it. So "unchanged since our
/// paste" means "still exactly our paste". (A process without clipboard
/// access reads 0 both times — equal — and we restore, as v1 always did.)
fn restore_decision(pasted: u32, now: u32) -> Restore {
    if now == pasted {
        Restore::PutBack
    } else {
        Restore::LeaveIt
    }
}

/// Put the user's old text back, if the clipboard is still ours to give.
fn restore(saved: String, pasted: u32) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return;
    };
    // arboard opens the clipboard the moment the builder exists (3.6's
    // `Set::new`) and keeps it open until the builder is spent or dropped,
    // so nobody else can write it in between. Reading the number INSIDE
    // that window means no copy can land between our check and our write.
    // (If arboard ever opened lazily, this would shrink to a race a few
    // milliseconds wide, not a bug.)
    let set = clipboard.set();
    match restore_decision(pasted, sequence_number()) {
        // Marked like the paste. The user's text is already in their
        // history if it was ever allowed there — and if it came from a
        // password manager, it was marked to stay out. Restoring it
        // unmarked would launder it into Win+V and the cloud.
        Restore::PutBack => {
            let _ = unlisted(set).text(saved);
        }
        Restore::LeaveIt => {
            drop(set);
            println!("[paste] clipboard changed since the paste; keeping the new copy");
        }
    }
}

// ───────── 3. hands off first ─────────

/// Is `key` down right now? GetAsyncKeyState reads the keyboard as it is
/// this instant — not as some window's message queue last saw it — and
/// sets the top bit (so: a negative number) while the key is down.
fn key_down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key)) < 0 }
}

/// The conflicting modifiers down right now, by name, each name once.
fn held(is_down: &impl Fn(u16) -> bool) -> Vec<&'static str> {
    let mut names = Vec::new();
    for (key, name) in CONFLICTING_MODIFIERS {
        if is_down(key) && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// How the wait before Ctrl+V ended — the log line wants to know.
#[derive(Debug, PartialEq)]
enum Hands {
    /// Nothing held when the settle ended: the everyday case, no time added.
    Free,
    /// These were held when the settle ended; we waited until all came up.
    LetGo(Vec<&'static str>),
    /// Still held at HOLD_LIMIT; we paste anyway. Never do we fake their
    /// release — Windows would think the key is up while a finger is on it.
    StillHeld(Vec<&'static str>),
}

/// The settle, stretched only while a modifier is physically down. The
/// clipboard needs SETTLE no matter what; after it, a held modifier keeps
/// us waiting, looking every POLL, until HOLD_LIMIT (counted from the
/// start, settle included). So a key let go during the settle costs
/// nothing, and so does a key nobody is holding.
///
/// Everything it touches comes in as a parameter — the keyboard
/// (`is_down`), the clock (`elapsed`), and `sleep` — so the tests can run
/// it against a fake keyboard on a fake clock, in microseconds.
/// Production passes GetAsyncKeyState, an Instant, and thread::sleep.
fn wait_to_paste(
    is_down: impl Fn(u16) -> bool,
    elapsed: impl Fn() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Hands {
    sleep(SETTLE);
    let mut holding = held(&is_down);
    if holding.is_empty() {
        return Hands::Free;
    }
    let first = holding.clone();
    while elapsed() < HOLD_LIMIT {
        sleep(POLL);
        holding = held(&is_down);
        if holding.is_empty() {
            return Hands::LetGo(first);
        }
    }
    Hands::StillHeld(holding)
}

// ───────── 4. a real Ctrl+V ─────────

/// One half of a keystroke, as plain data so the chord can be tested:
/// which virtual key, which scan code (the physical key), and whether it
/// is going down or coming up.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stroke {
    key: u16,
    scan: u16,
    up: bool,
}

impl Stroke {
    /// The SendInput form. No KEYEVENTF_SCANCODE: Windows routes on the
    /// virtual key, and the scan code rides along in the key message —
    /// exactly where a physical keypress puts it.
    fn input(self) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: self.key,
                    wScan: self.scan,
                    dwFlags: if self.up { KEYEVENTF_KEYUP } else { 0 },
                    // 0 = let Windows stamp the event with the current time.
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

/// The paste chord, pure so it can be tested: Ctrl down, V down, V up,
/// Ctrl up — unless the user is already holding Ctrl (`ctrl_held`). Then
/// their Ctrl is half the chord: pressing it again is pointless, and
/// releasing it would be a lie Windows believes while their finger is
/// still on it. Each key carries the scan code `scan` gives it.
fn ctrl_v(ctrl_held: bool, scan: impl Fn(u16) -> u16) -> Vec<Stroke> {
    let down = |key| Stroke {
        key,
        scan: scan(key),
        up: false,
    };
    let up = |key| Stroke {
        key,
        scan: scan(key),
        up: true,
    };
    if ctrl_held {
        vec![down(VK_V), up(VK_V)]
    } else {
        vec![down(VK_CONTROL), down(VK_V), up(VK_V), up(VK_CONTROL)]
    }
}

/// If Windows took only the first `sent` strokes of `chord`, these are the
/// releases still owed: an up for every key the accepted part pressed and
/// didn't let go. Never a key we didn't press — so never one the user holds.
fn owed_releases(chord: &[Stroke], sent: usize) -> Vec<Stroke> {
    let (done, rest) = chord.split_at(sent.min(chord.len()));
    rest.iter()
        .filter(|s| s.up && done.iter().any(|d| !d.up && d.key == s.key))
        .copied()
        .collect()
}

/// The keyboard layout of the window the paste is for. Layouts are per
/// thread on Windows — the app being dictated into may be in Russian while
/// sayit's threads are in English — so ask the foreground window's thread,
/// not ours. With no foreground window (a locked screen), its thread id is
/// 0, which GetKeyboardLayout reads as "the calling thread": the best guess
/// left.
fn foreground_layout() -> HKL {
    unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
        GetKeyboardLayout(thread)
    }
}

/// The scan code — which physical key — `key` lives on in `layout`. A real
/// keyboard sends both. Windows routes on the virtual key, but
/// Chromium-based apps (VS Code, browsers, Slack, Discord) build
/// `KeyboardEvent.code` from the scan code, and VS Code matches its
/// keybindings on that code: hex once sent Ctrl+V with no scan code, and
/// VS Code's terminal passed a literal ^V to the shell (hex #618).
fn scan_code(key: u16, layout: HKL) -> u16 {
    unsafe { MapVirtualKeyExW(u32::from(key), MAPVK_VK_TO_VSC, layout) as u16 }
}

/// Hand strokes to SendInput in ONE call; returns how many it accepted.
/// One call matters: Windows delivers a call's events back to back, so no
/// physical keystroke can land between our Ctrl-down and our V.
fn send(strokes: &[Stroke]) -> usize {
    if strokes.is_empty() {
        return 0;
    }
    let inputs: Vec<INPUT> = strokes.iter().map(|s| s.input()).collect();
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) as usize
    }
}

fn press_ctrl_v() -> Result<(), String> {
    let layout = foreground_layout();
    let chord = ctrl_v(key_down(VK_CONTROL), |key| scan_code(key, layout));
    let sent = send(&chord);
    if sent < chord.len() {
        // Don't leave a half-pressed chord behind.
        send(&owed_releases(&chord, sent));
        return Err(format!(
            "Windows accepted {sent} of {} paste keystrokes (an elevated window or a desktop switch can refuse synthetic input)",
            chord.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ── restore ──

    #[test]
    fn an_untouched_clipboard_gets_its_old_text_back() {
        assert_eq!(restore_decision(41, 41), Restore::PutBack);
    }

    #[test]
    fn a_copy_made_after_the_paste_is_never_overwritten() {
        assert_eq!(restore_decision(41, 42), Restore::LeaveIt);
        assert_eq!(restore_decision(41, 97), Restore::LeaveIt);
    }

    #[test]
    fn a_wrapped_counter_still_reads_as_changed() {
        assert_eq!(restore_decision(u32::MAX, 0), Restore::LeaveIt);
    }

    #[test]
    fn no_clipboard_access_falls_back_to_restoring() {
        // GetClipboardSequenceNumber answers 0 without clipboard access.
        assert_eq!(restore_decision(0, 0), Restore::PutBack);
    }

    // ── hands off ──

    /// Run the wait on a fake clock: `sleep` moves time forward, and the
    /// fake keyboard sees what time it is. Returns how the wait ended and
    /// when.
    fn wait_on_fake_clock(keyboard: impl Fn(u16, Duration) -> bool) -> (Hands, Duration) {
        let now = Cell::new(Duration::ZERO);
        let hands = wait_to_paste(
            |key| keyboard(key, now.get()),
            || now.get(),
            |d| now.set(now.get() + d),
        );
        (hands, now.get())
    }

    #[test]
    fn nobody_holding_anything_costs_only_the_settle() {
        assert_eq!(wait_on_fake_clock(|_, _| false), (Hands::Free, SETTLE));
    }

    #[test]
    fn a_modifier_let_go_during_the_settle_costs_nothing() {
        let shift_until_30ms = |key: u16, t: Duration| key == VK_SHIFT && t < ms(30);
        assert_eq!(wait_on_fake_clock(shift_until_30ms), (Hands::Free, SETTLE));
    }

    #[test]
    fn a_held_shift_holds_the_paste_until_it_comes_up() {
        let shift_until_250ms = |key: u16, t: Duration| key == VK_SHIFT && t < ms(250);
        assert_eq!(
            wait_on_fake_clock(shift_until_250ms),
            (Hands::LetGo(vec!["Shift"]), ms(250))
        );
    }

    #[test]
    fn the_wait_lasts_until_the_last_modifier_is_up() {
        let keyboard = |key: u16, t: Duration| {
            (key == VK_SHIFT && t < ms(100)) || (key == VK_MENU && t < ms(400))
        };
        assert_eq!(
            wait_on_fake_clock(keyboard),
            (Hands::LetGo(vec!["Shift", "Alt"]), ms(400))
        );
    }

    #[test]
    fn a_stuck_modifier_gives_up_at_the_limit() {
        let win_forever = |key: u16, _: Duration| key == VK_LWIN;
        assert_eq!(
            wait_on_fake_clock(win_forever),
            (Hands::StillHeld(vec!["Win"]), HOLD_LIMIT)
        );
    }

    #[test]
    fn a_held_ctrl_never_delays_the_paste() {
        let ctrl_forever = |key: u16, _: Duration| key == VK_CONTROL;
        assert_eq!(wait_on_fake_clock(ctrl_forever), (Hands::Free, SETTLE));
    }

    #[test]
    fn both_win_keys_are_named_once() {
        let both_wins = |key: u16| key == VK_LWIN || key == VK_RWIN;
        assert_eq!(held(&both_wins), vec!["Win"]);
    }

    // ── the chord ──

    /// US-layout scan codes for the chord's two keys.
    fn us_scan(key: u16) -> u16 {
        match key {
            VK_CONTROL => 0x1D,
            VK_V => 0x2F,
            _ => 0,
        }
    }

    fn down(key: u16) -> Stroke {
        Stroke {
            key,
            scan: us_scan(key),
            up: false,
        }
    }

    fn up(key: u16) -> Stroke {
        Stroke {
            key,
            scan: us_scan(key),
            up: true,
        }
    }

    #[test]
    fn the_chord_is_ctrl_wrapped_around_v_with_scan_codes() {
        assert_eq!(
            ctrl_v(false, us_scan),
            [down(VK_CONTROL), down(VK_V), up(VK_V), up(VK_CONTROL)]
        );
    }

    #[test]
    fn a_held_ctrl_is_neither_pressed_nor_released() {
        assert_eq!(ctrl_v(true, us_scan), [down(VK_V), up(VK_V)]);
    }

    #[test]
    fn strokes_become_key_events_with_their_scan_code_and_direction() {
        let ki = unsafe { up(VK_V).input().Anonymous.ki };
        assert_eq!(
            (ki.wVk, ki.wScan, ki.dwFlags),
            (VK_V, 0x2F, KEYEVENTF_KEYUP)
        );
        let ki = unsafe { down(VK_CONTROL).input().Anonymous.ki };
        assert_eq!((ki.wVk, ki.wScan, ki.dwFlags), (VK_CONTROL, 0x1D, 0));
    }

    #[test]
    fn a_partial_send_releases_exactly_what_it_pressed() {
        let chord = ctrl_v(false, us_scan);
        assert!(owed_releases(&chord, 0).is_empty());
        assert_eq!(owed_releases(&chord, 1), [up(VK_CONTROL)]);
        assert_eq!(owed_releases(&chord, 2), [up(VK_V), up(VK_CONTROL)]);
        assert_eq!(owed_releases(&chord, 3), [up(VK_CONTROL)]);
        assert!(owed_releases(&chord, 4).is_empty());
    }

    #[test]
    fn a_partial_send_never_releases_the_users_ctrl() {
        let chord = ctrl_v(true, us_scan);
        assert_eq!(owed_releases(&chord, 1), [up(VK_V)]);
    }

    // ── the real clipboard (opt-in) ──

    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW,
    };

    const MARKS: [&str; 3] = [
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ];

    /// A registered clipboard format's id, by name.
    fn format_id(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    }

    /// True when the clipboard holds nothing, or only plain text (plus the
    /// flavours Windows derives from it, plus our own marks) — the only
    /// states this test can put back exactly. Lists format ids, never
    /// reads contents.
    fn plain_or_empty() -> bool {
        // CF_TEXT, CF_OEMTEXT, CF_UNICODETEXT, CF_LOCALE.
        let mut restorable = vec![1, 7, 13, 16];
        restorable.extend(MARKS.map(format_id));
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return false;
            }
            let mut plain = true;
            let mut format = EnumClipboardFormats(0);
            while format != 0 {
                plain &= restorable.contains(&format);
                format = EnumClipboardFormats(format);
            }
            CloseClipboard();
            plain
        }
    }

    /// What an older app's paste does: ask for CF_TEXT (1), the ANSI
    /// flavour Windows converts from our Unicode text on demand.
    fn read_like_an_ansi_app() -> bool {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return false;
            }
            let got = !GetClipboardData(1).is_null();
            CloseClipboard();
            got
        }
    }

    /// The one test that touches the REAL clipboard, so it's opt-in:
    /// `cargo test real_clipboard -- --ignored`. It puts your text back
    /// when done, and won't start over anything it couldn't put back
    /// exactly (an image, files, rich text from a browser or an editor).
    #[test]
    #[ignore = "touches the real clipboard"]
    fn real_clipboard_carries_the_marks_and_a_paste_is_not_a_copy() {
        if !plain_or_empty() {
            println!("skipped: the clipboard holds more than plain text, which this test can't restore exactly");
            return;
        }
        let mut clipboard = arboard::Clipboard::new().unwrap();
        let before = clipboard.get_text().ok();

        // Nothing between here and the restore may panic, or the test
        // text would be stranded on the clipboard.
        let set_ok = unlisted(clipboard.set())
            .text("sayit clipboard test")
            .is_ok();
        let pasted = sequence_number();
        let marks = MARKS.map(|name| unsafe { IsClipboardFormatAvailable(format_id(name)) != 0 });
        let read = clipboard.get_text().ok();
        let ansi_read = read_like_an_ansi_app();
        let after_reads = sequence_number();

        match before.clone() {
            Some(text) => restore(text, after_reads),
            None => {
                let _ = clipboard.clear();
            }
        }
        let restored = clipboard.get_text().ok();

        assert!(set_ok, "couldn't put the test text on the clipboard");
        assert_eq!(marks, [true; 3], "marks present: {MARKS:?}");
        assert_eq!(read.as_deref(), Some("sayit clipboard test"));
        assert!(ansi_read, "couldn't read the ANSI flavour");
        assert_eq!(
            after_reads, pasted,
            "reading the clipboard bumped its sequence number"
        );
        assert_eq!(restored, before, "the old clipboard didn't come back");
    }
}
