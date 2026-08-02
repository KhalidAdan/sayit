//! Stage 1: the key. Windows keeps the physical hold-F9 gesture. Linux
//! listens to evdev for a clean double-tap of left Control to start and one
//! clean tap to stop. Both backends collapse OS input into the same two
//! events consumed by the coordinator.

use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

/// The Windows push-to-talk key. Linux advertises its gesture separately.
#[cfg(windows)]
pub const PUSH_TO_TALK: &str = "F9";

const DOUBLE_TAP_MS: u64 = 400;
/// Composite USB keyboards can publish the same physical key through more
/// than one event node. Kernel timestamps let us collapse those copies without
/// making a genuinely fast human tap disappear.
const DUPLICATE_EVENT_MS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerMode {
    Idle,
    Starting,
    Recording,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyTransition {
    LeftDown,
    LeftUp,
    OtherDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gesture {
    Started,
    Finished,
}

/// Pure gesture state. It sees only three facts — left-Control down/up and
/// "some other key went down" — so no key names or typed content can leak
/// into logs or persistence.
#[derive(Debug)]
struct GestureMachine {
    mode: TriggerMode,
    left_down: bool,
    clean: bool,
    first_tap_ms: Option<u64>,
}

impl Default for GestureMachine {
    fn default() -> Self {
        Self {
            mode: TriggerMode::Idle,
            left_down: false,
            clean: false,
            first_tap_ms: None,
        }
    }
}

impl GestureMachine {
    fn set_mode(&mut self, mode: TriggerMode) {
        self.mode = mode;
        self.first_tap_ms = None;
        // Do not carry a half-observed key across a coordinator transition.
        self.left_down = false;
        self.clean = false;
    }

    fn event(&mut self, transition: KeyTransition, at_ms: u64) -> Option<Gesture> {
        match transition {
            KeyTransition::LeftDown => {
                self.left_down = true;
                self.clean = true;
                None
            }
            KeyTransition::OtherDown => {
                if self.left_down {
                    self.clean = false;
                }
                // A normal shortcut or any typing between taps breaks the
                // double-tap sequence. Ctrl, C, Ctrl must never start sayit.
                self.first_tap_ms = None;
                None
            }
            KeyTransition::LeftUp => {
                let completed = self.left_down && self.clean;
                self.left_down = false;
                self.clean = false;
                if !completed {
                    return None;
                }
                match self.mode {
                    TriggerMode::Recording => {
                        self.mode = TriggerMode::Busy;
                        self.first_tap_ms = None;
                        Some(Gesture::Finished)
                    }
                    TriggerMode::Idle => match self.first_tap_ms.take() {
                        Some(first) if at_ms.saturating_sub(first) <= DOUBLE_TAP_MS => {
                            self.mode = TriggerMode::Starting;
                            Some(Gesture::Started)
                        }
                        _ => {
                            self.first_tap_ms = Some(at_ms);
                            None
                        }
                    },
                    TriggerMode::Starting | TriggerMode::Busy => None,
                }
            }
        }
    }
}

#[derive(Default)]
struct HotkeyState {
    gesture: GestureMachine,
    last_left_down_ms: Option<u64>,
    last_left_up_ms: Option<u64>,
    last_other_down_ms: Option<u64>,
}

impl HotkeyState {
    fn event(&mut self, transition: KeyTransition, at_ms: u64) -> Option<Gesture> {
        let last = match transition {
            KeyTransition::LeftDown => &mut self.last_left_down_ms,
            KeyTransition::LeftUp => &mut self.last_left_up_ms,
            KeyTransition::OtherDown => &mut self.last_other_down_ms,
        };
        if last.is_some_and(|previous| at_ms.saturating_sub(previous) <= DUPLICATE_EVENT_MS) {
            return None;
        }
        *last = Some(at_ms);
        self.gesture.event(transition, at_ms)
    }
}

#[derive(Default)]
pub struct HotkeyControl(Mutex<HotkeyState>);

pub fn set_mode(control: &HotkeyControl, mode: TriggerMode) {
    // Keep the duplicate timestamps across this transition: another interface
    // may deliver the same physical release after the coordinator has already
    // switched to Recording. Dropping it here can turn that copy into Stop.
    control.0.lock().unwrap().gesture.set_mode(mode);
}

/// Wall-clock milliseconds, stamped where the OS event is observed and
/// carried to the webview for end-to-end gap accounting.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri_plugin_global_shortcut::ShortcutState;

    static HELD: AtomicBool = AtomicBool::new(false);

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
}

#[cfg(windows)]
pub use windows::on_shortcut;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use evdev::{enumerate, EventSummary, KeyCode};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};
    use tauri::Manager;

    fn caps_is_control() -> bool {
        if std::env::var_os("SAYIT_CAPS_AS_CTRL").is_some() {
            return true;
        }
        // GNOME applies XKB after evdev. At this layer a remapped Caps key is
        // still KEY_CAPSLOCK, so mirror the effective options explicitly.
        let output = std::process::Command::new("/usr/bin/gsettings")
            .args(["get", "org.gnome.desktop.input-sources", "xkb-options"])
            .output();
        let Ok(output) = output else { return false };
        let options = String::from_utf8_lossy(&output.stdout);
        [
            "ctrl:nocaps",
            "ctrl:swapcaps",
            "ctrl:grouptoggle_capscontrol",
            "ctrl:hyper_capscontrol",
        ]
        .iter()
        .any(|option| options.contains(option))
    }

    fn is_keyboard(device: &evdev::Device) -> bool {
        let name = device.name().unwrap_or_default();
        if name.starts_with("sayit ") || device.supported_relative_axes().is_some() {
            return false;
        }
        device.supported_keys().is_some_and(|keys| {
            keys.contains(KeyCode::KEY_LEFTCTRL)
                && keys.contains(KeyCode::KEY_A)
                && keys.contains(KeyCode::KEY_SPACE)
        })
    }

    fn spawn_device(
        path: PathBuf,
        mut device: evdev::Device,
        app: AppHandle,
        active: Arc<Mutex<HashSet<PathBuf>>>,
        epoch: Instant,
        caps_as_control: bool,
    ) {
        std::thread::spawn(move || {
            println!(
                "[input] listening to {} ({})",
                device.name().unwrap_or("keyboard"),
                path.display()
            );
            loop {
                let events = match device.fetch_events() {
                    Ok(events) => events,
                    Err(e) => {
                        eprintln!("[input] {} disconnected: {e}", path.display());
                        break;
                    }
                };
                for event in events {
                    let transition = match event.destructure() {
                        EventSummary::Key(_, key, 1)
                            if key == KeyCode::KEY_LEFTCTRL
                                || (caps_as_control && key == KeyCode::KEY_CAPSLOCK) =>
                        {
                            Some(KeyTransition::LeftDown)
                        }
                        EventSummary::Key(_, key, 0)
                            if key == KeyCode::KEY_LEFTCTRL
                                || (caps_as_control && key == KeyCode::KEY_CAPSLOCK) =>
                        {
                            Some(KeyTransition::LeftUp)
                        }
                        // Value 2 is key repeat and deliberately ignored.
                        EventSummary::Key(_, key, 1)
                            if key != KeyCode::KEY_LEFTCTRL
                                && !(caps_as_control && key == KeyCode::KEY_CAPSLOCK) =>
                        {
                            Some(KeyTransition::OtherDown)
                        }
                        _ => None,
                    };
                    let Some(transition) = transition else {
                        continue;
                    };
                    let elapsed = event
                        .timestamp()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_millis() as u64)
                        .unwrap_or_else(|_| epoch.elapsed().as_millis() as u64);
                    let gesture = app
                        .state::<HotkeyControl>()
                        .0
                        .lock()
                        .unwrap()
                        .event(transition, elapsed);
                    match gesture {
                        Some(Gesture::Started) => {
                            let _ = app.emit("push_started", now_ms());
                        }
                        Some(Gesture::Finished) => {
                            let _ = app.emit("push_finished", now_ms());
                        }
                        None => {}
                    }
                }
            }
            active.lock().unwrap().remove(&path);
        });
    }

    /// Starts a hotplug-aware evdev listener. It never EVIOCGRABs: every key
    /// continues to reach GNOME and the focused application normally.
    pub fn start(app: &AppHandle) -> Result<usize, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let app = app.clone();
        let caps_as_control = caps_is_control();
        if caps_as_control {
            println!("[input] GNOME ctrl:nocaps detected; Caps Lock is a trigger key");
        }
        std::thread::spawn(move || {
            let active = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));
            let epoch = Instant::now();
            let mut first_scan = true;
            loop {
                let mut found = 0usize;
                for (path, device) in enumerate() {
                    if !is_keyboard(&device) {
                        continue;
                    }
                    found += 1;
                    let inserted = active.lock().unwrap().insert(path.clone());
                    if inserted {
                        spawn_device(
                            path,
                            device,
                            app.clone(),
                            active.clone(),
                            epoch,
                            caps_as_control,
                        );
                    }
                }
                if first_scan {
                    let _ = ready_tx.send(found);
                    first_scan = false;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(0) => Err("no readable keyboard event device found".into()),
            Ok(count) => Ok(count),
            Err(_) => Err("keyboard device scan timed out".into()),
        }
    }

    pub fn probe() -> Result<Vec<String>, String> {
        let devices: Vec<String> = enumerate()
            .filter_map(|(path, device)| {
                is_keyboard(&device).then(|| {
                    format!(
                        "{} ({})",
                        device.name().unwrap_or("keyboard"),
                        path.display()
                    )
                })
            })
            .collect();
        if devices.is_empty() {
            Err("no readable keyboard event device found".into())
        } else {
            Ok(devices)
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{probe as probe_linux, start as start_linux};

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(machine: &mut GestureMachine, at: u64) -> Option<Gesture> {
        assert_eq!(machine.event(KeyTransition::LeftDown, at - 1), None);
        machine.event(KeyTransition::LeftUp, at)
    }

    #[test]
    fn clean_double_tap_starts() {
        let mut m = GestureMachine::default();
        assert_eq!(tap(&mut m, 100), None);
        assert_eq!(tap(&mut m, 450), Some(Gesture::Started));
        assert_eq!(m.mode, TriggerMode::Starting);
    }

    #[test]
    fn slow_double_tap_does_not_start() {
        let mut m = GestureMachine::default();
        assert_eq!(tap(&mut m, 100), None);
        assert_eq!(tap(&mut m, 501), None);
        assert_eq!(m.mode, TriggerMode::Idle);
    }

    #[test]
    fn chord_is_not_a_tap() {
        let mut m = GestureMachine::default();
        m.event(KeyTransition::LeftDown, 10);
        m.event(KeyTransition::OtherDown, 11);
        assert_eq!(m.event(KeyTransition::LeftUp, 12), None);
        assert_eq!(tap(&mut m, 100), None);
    }

    #[test]
    fn typing_between_taps_resets_sequence() {
        let mut m = GestureMachine::default();
        assert_eq!(tap(&mut m, 100), None);
        m.event(KeyTransition::OtherDown, 200);
        assert_eq!(tap(&mut m, 300), None);
    }

    #[test]
    fn one_clean_tap_finishes_recording() {
        let mut m = GestureMachine::default();
        m.set_mode(TriggerMode::Recording);
        assert_eq!(tap(&mut m, 100), Some(Gesture::Finished));
        assert_eq!(m.mode, TriggerMode::Busy);
        assert_eq!(tap(&mut m, 200), None);
    }

    #[test]
    fn coordinator_reset_discards_partial_sequence() {
        let mut m = GestureMachine::default();
        assert_eq!(tap(&mut m, 100), None);
        m.set_mode(TriggerMode::Idle);
        assert_eq!(tap(&mut m, 200), None);
    }

    #[test]
    fn duplicate_event_nodes_count_as_one_physical_tap() {
        let mut state = HotkeyState::default();
        assert_eq!(state.event(KeyTransition::LeftDown, 100), None);
        assert_eq!(state.event(KeyTransition::LeftDown, 101), None);
        assert_eq!(state.event(KeyTransition::LeftUp, 170), None);
        assert_eq!(state.event(KeyTransition::LeftUp, 171), None);
        assert_eq!(state.event(KeyTransition::LeftDown, 300), None);
        assert_eq!(state.event(KeyTransition::LeftDown, 301), None);
        assert_eq!(
            state.event(KeyTransition::LeftUp, 370),
            Some(Gesture::Started)
        );
        assert_eq!(state.event(KeyTransition::LeftUp, 371), None);
    }

    #[test]
    fn duplicate_release_after_recording_transition_does_not_stop() {
        let mut state = HotkeyState::default();
        state.last_left_up_ms = Some(100);
        state.gesture.set_mode(TriggerMode::Recording);
        assert_eq!(state.event(KeyTransition::LeftUp, 101), None);
        assert_eq!(state.gesture.mode, TriggerMode::Recording);
    }
}
