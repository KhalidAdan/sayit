//! The Linux trigger: a hotplug-aware evdev listener, one thread per
//! keyboard device, feeding raw key transitions into the shared
//! GestureMachine (hotkey.rs). Double-tap left Control starts a take, one
//! clean tap stops it.

use crate::hotkey::{now_ms, Gesture, HotkeyControl, KeyTransition};
use evdev::{enumerate, EventSummary, KeyCode};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

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
                let gesture = app.state::<HotkeyControl>().event(transition, elapsed);
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
pub(super) fn start(app: &AppHandle) -> Result<usize, String> {
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

pub(super) fn probe() -> Result<Vec<String>, String> {
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
