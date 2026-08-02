//! Stage 1: the key — the OS-free half. The pure gesture state machine
//! that turns raw key transitions into `push_started` / `push_finished`,
//! shared by every platform backend. How those transitions are observed
//! (global-shortcut plugin on Windows, evdev on Linux) lives in
//! `platform/`; this module never touches the OS, which is why all eight
//! of its tests run everywhere.

// On Windows the machine is compiled but unfed — the global-shortcut
// plugin reports press/release directly and never produces KeyTransitions,
// so dead-code analysis fires there. Linux feeds it for real, and the
// tests exercise it on every target.
#![allow(dead_code)]

use std::sync::Mutex;

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
pub(crate) enum KeyTransition {
    LeftDown,
    LeftUp,
    OtherDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gesture {
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

impl HotkeyControl {
    /// Feed one raw key transition through duplicate-coalescing and the
    /// gesture machine. Platform listeners call this; the pure state stays
    /// in here where the tests are.
    pub(crate) fn event(&self, transition: KeyTransition, at_ms: u64) -> Option<Gesture> {
        self.0.lock().unwrap().event(transition, at_ms)
    }
}

pub fn set_mode(control: &HotkeyControl, mode: TriggerMode) {
    // Keep the duplicate timestamps across this transition: another interface
    // may deliver the same physical release after the coordinator has already
    // switched to Recording. Dropping it here can turn that copy into Stop.
    control.0.lock().unwrap().gesture.set_mode(mode);
}

/// Wall-clock milliseconds, stamped where the OS event is observed and
/// carried to the webview for end-to-end gap accounting.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
