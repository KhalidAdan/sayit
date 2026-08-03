//! Shared waveform geometry. The desired place is platform-neutral; when a
//! native host learns its real size is a platform concern handled behind
//! `Platform`.

use std::sync::Mutex;

const BOTTOM_GAP: u32 = 96;

/// Native configure streams may repeat a size when only position changed.
/// Only a genuinely new size may trigger another placement.
///
/// Only the Linux backend listens to configure events, so a Windows build
/// never constructs this. `allow` instead of a cfg: OS cfgs live in
/// platform/mod.rs alone (the CI tripwire).
#[allow(dead_code)]
pub struct SizeTracker(Mutex<Option<tauri::PhysicalSize<u32>>>);

#[allow(dead_code)]
impl SizeTracker {
    pub fn new(initial: Option<tauri::PhysicalSize<u32>>) -> Self {
        Self(Mutex::new(initial))
    }

    pub fn changed(&self, size: tauri::PhysicalSize<u32>) -> bool {
        let mut previous = self.0.lock().unwrap();
        if previous.as_ref() == Some(&size) {
            false
        } else {
            *previous = Some(size);
            true
        }
    }
}

fn add_offset(origin: i32, offset: u32) -> i32 {
    origin.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX))
}

fn bottom_center(
    origin: tauri::PhysicalPosition<i32>,
    screen: tauri::PhysicalSize<u32>,
    host: tauri::PhysicalSize<u32>,
) -> tauri::PhysicalPosition<i32> {
    let x = screen.width.saturating_sub(host.width) / 2;
    let y = screen
        .height
        .saturating_sub(host.height.saturating_add(BOTTOM_GAP));
    tauri::PhysicalPosition::new(add_offset(origin.x, x), add_offset(origin.y, y))
}

pub fn position(window: &tauri::WebviewWindow, host: tauri::PhysicalSize<u32>) {
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let target = bottom_center(*monitor.position(), *monitor.size(), host);
        let _ = window.set_position(target);
    }
}

pub fn position_reported_size(window: &tauri::WebviewWindow) {
    if let Ok(size) = window.outer_size() {
        position(window, size);
    }
}

/// The default host behavior. Platforms with delayed native sizing override
/// `Platform::show_waveform`; a normal host can be placed before it is shown.
///
/// The pill must NEVER take focus, or the paste lands in the wrong place —
/// and since it stays up through the paste, that rule is load-bearing. Two
/// settings in tauri.conf.json hold it:
/// - `focusable: false`: WS_EX_NOACTIVATE on Windows, so a click can't
///   activate it;
/// - `focus: false`: tao shows the window with SW_SHOWNOACTIVATE instead
///   of SW_SHOW, which would activate it.
///
/// Click-through is deferred until it's verified on real Windows: tao's
/// `set_ignore_cursor_events(true)` adds WS_EX_TRANSPARENT | WS_EX_LAYERED,
/// and nothing in tao or wry calls SetLayeredWindowAttributes — per the
/// Win32 docs, a layered window left that way may not be drawn at all. So
/// a click on the pill still lands on the pill (without moving focus).
pub fn show(window: &tauri::WebviewWindow) {
    position_reported_size(window);
    let _ = window.show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_requested_host_ninety_six_pixels_above_bottom() {
        let point = bottom_center(
            tauri::PhysicalPosition::new(0, 0),
            tauri::PhysicalSize::new(1920, 1080),
            tauri::PhysicalSize::new(280, 64),
        );
        assert_eq!(point, tauri::PhysicalPosition::new(820, 920));
    }

    #[test]
    fn uses_real_oversized_host_height() {
        let point = bottom_center(
            tauri::PhysicalPosition::new(0, 0),
            tauri::PhysicalSize::new(1920, 1080),
            tauri::PhysicalSize::new(280, 200),
        );
        assert_eq!(point, tauri::PhysicalPosition::new(820, 784));
    }

    #[test]
    fn includes_monitor_origin() {
        let point = bottom_center(
            tauri::PhysicalPosition::new(-1920, 240),
            tauri::PhysicalSize::new(1920, 1080),
            tauri::PhysicalSize::new(280, 64),
        );
        assert_eq!(point, tauri::PhysicalPosition::new(-1100, 1160));
    }

    #[test]
    fn configure_moves_do_not_repeat_the_same_size() {
        let tracker = SizeTracker::new(Some(tauri::PhysicalSize::new(280, 64)));
        assert!(!tracker.changed(tauri::PhysicalSize::new(280, 64)));
        assert!(tracker.changed(tauri::PhysicalSize::new(280, 200)));
        assert!(!tracker.changed(tauri::PhysicalSize::new(280, 200)));
    }
}
