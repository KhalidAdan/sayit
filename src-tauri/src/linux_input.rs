//! Linux output side of the key: one process-owned uinput keyboard plus a
//! long-lived Wayland clipboard owner. No ydotool daemon and no root process.

use crate::inject::InjectTiming;
use arboard::{ClearExtLinux, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use evdev::{uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};

struct Request {
    text: String,
    answer: Sender<Result<InjectTiming, String>>,
}

#[derive(Clone)]
pub struct LinuxInput {
    tx: Sender<Request>,
}

impl LinuxInput {
    pub fn start() -> Self {
        let (tx, rx) = channel::<Request>();
        std::thread::spawn(move || {
            let mut keyboard = VirtualKeyboard::new();
            let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string());
            while let Ok(request) = rx.recv() {
                let result = match (&mut keyboard, &mut clipboard) {
                    (Ok(keyboard), Ok(clipboard)) => inject(keyboard, clipboard, &request.text),
                    (Err(e), _) => Err(e.clone()),
                    (_, Err(e)) => Err(e.clone()),
                };
                let _ = request.answer.send(result);
            }
        });
        Self { tx }
    }

    pub fn inject(&self, text: String) -> Result<InjectTiming, String> {
        let (answer, response) = channel();
        self.tx
            .send(Request { text, answer })
            .map_err(|_| "Linux input worker stopped".to_string())?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "Linux input worker did not answer".to_string())?
    }
}

struct VirtualKeyboard(VirtualDevice);

impl VirtualKeyboard {
    fn new() -> Result<Self, String> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_LEFTSHIFT);
        keys.insert(KeyCode::KEY_INSERT);
        let device = VirtualDevice::builder()
            .map_err(|e| format!("cannot open /dev/uinput: {e}"))?
            .name("sayit virtual keyboard")
            .with_keys(&keys)
            .map_err(|e| format!("cannot configure /dev/uinput: {e}"))?
            .build()
            .map_err(|e| format!("cannot create virtual keyboard: {e}"))?;
        Ok(Self(device))
    }

    fn shift_insert(&mut self) -> Result<(), String> {
        let key = |code: KeyCode, value| InputEvent::new(EventType::KEY.0, code.code(), value);
        self.0
            .emit(&[key(KeyCode::KEY_LEFTSHIFT, 1), key(KeyCode::KEY_INSERT, 1)])
            .map_err(|e| format!("uinput key press failed: {e}"))?;
        std::thread::sleep(Duration::from_millis(5));
        self.0
            .emit(&[key(KeyCode::KEY_INSERT, 0), key(KeyCode::KEY_LEFTSHIFT, 0)])
            .map_err(|e| format!("uinput key release failed: {e}"))?;
        Ok(())
    }
}

fn read_selection(
    clipboard: &mut arboard::Clipboard,
    selection: LinuxClipboardKind,
) -> Option<String> {
    clipboard.get().clipboard(selection).text().ok()
}

fn write_selection(
    clipboard: &mut arboard::Clipboard,
    selection: LinuxClipboardKind,
    text: &str,
) -> Result<(), String> {
    clipboard
        .set()
        .clipboard(selection)
        .text(text.to_owned())
        .map_err(|e| e.to_string())
}

fn restore_selection(
    clipboard: &mut arboard::Clipboard,
    selection: LinuxClipboardKind,
    injected: &str,
    saved: Option<String>,
) {
    // Never overwrite something the user copied during our polite restore
    // delay. If the destination or user changed the selection, it now owns it.
    if read_selection(clipboard, selection).as_deref() != Some(injected) {
        return;
    }
    if let Some(saved) = saved {
        let _ = write_selection(clipboard, selection, &saved);
    } else {
        let _ = clipboard.clear_with().clipboard(selection);
    }
}

fn inject(
    keyboard: &mut VirtualKeyboard,
    clipboard: &mut arboard::Clipboard,
    text: &str,
) -> Result<InjectTiming, String> {
    let t_all = Instant::now();

    let saved_clipboard = read_selection(clipboard, LinuxClipboardKind::Clipboard);
    let saved_primary = read_selection(clipboard, LinuxClipboardKind::Primary);
    let clipboard_save_ms = t_all.elapsed().as_millis() as u64;

    let t = Instant::now();
    write_selection(clipboard, LinuxClipboardKind::Clipboard, text)?;
    write_selection(clipboard, LinuxClipboardKind::Primary, text)?;
    let clipboard_set_ms = t.elapsed().as_millis() as u64;

    // Data-control selection ownership crosses a compositor boundary. Keep a
    // small measured settle, matching the trust-over-cleverness Windows path.
    let t = Instant::now();
    std::thread::sleep(Duration::from_millis(30));
    let settle_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    keyboard.shift_insert()?;
    let keystroke_ms = t.elapsed().as_millis() as u64;
    let visible_ms = t_all.elapsed().as_millis() as u64;

    let t = Instant::now();
    std::thread::sleep(Duration::from_millis(300));
    restore_selection(
        clipboard,
        LinuxClipboardKind::Clipboard,
        text,
        saved_clipboard,
    );
    restore_selection(clipboard, LinuxClipboardKind::Primary, text, saved_primary);
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

/// A side-effect-light setup probe: create and immediately destroy a virtual
/// keyboard without emitting any input.
pub fn probe_uinput() -> Result<(), String> {
    VirtualKeyboard::new().map(|_| ())
}
