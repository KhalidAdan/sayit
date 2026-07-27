//! The tray: sayit's only permanent visible presence. A status line, a
//! microphone picker, a start-with-Windows toggle, and Quit. Still no
//! settings screen — these are the key's physical switches, not a config.
//!
//! The status text is driven from the TypeScript coordinator (which owns
//! the state machine) through the `tray_status` command. Rust renders,
//! TS decides — same seam as everything else.

use std::sync::Mutex;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::{settings, MicChoice};

#[derive(Default)]
pub struct Tray {
    handles: Mutex<Option<(TrayIcon<Wry>, MenuItem<Wry>)>>,
}

pub fn build(app: &AppHandle, saved: &settings::Settings) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "warming up…", false, None::<&str>)?;

    // Microphone picker: system default plus every input device present at
    // boot, with the persisted choice pre-checked. (A saved mic that's
    // currently unplugged shows nothing checked but stays saved — capture
    // falls back to default until it returns.)
    let mut mic_items = vec![CheckMenuItem::with_id(
        app,
        "mic:",
        "System default",
        true,
        saved.microphone.is_none(),
        None::<&str>,
    )?];
    for name in crate::capture::list_inputs() {
        let chosen = saved.microphone.as_deref() == Some(name.as_str());
        mic_items.push(CheckMenuItem::with_id(
            app,
            format!("mic:{name}"),
            &name,
            true,
            chosen,
            None::<&str>,
        )?);
    }
    let mic_refs: Vec<&dyn IsMenuItem<Wry>> =
        mic_items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
    let mics = Submenu::with_id_and_items(app, "mics", "Microphone", true, &mic_refs)?;

    // Autostart only makes sense for the built app: enabling it from a dev
    // run would register the debug exe, which needs the vite server to be
    // useful. The toggle works either way; the caveat lives here as a
    // comment and in the docs.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start with Windows",
        true,
        autostart_on,
        None::<&str>,
    )?;

    // Engine override: auto-sleep is the default metabolism; this pins the
    // engine awake for heavy dictation days. The timer lives in the
    // coordinator, so this just reports the toggle upward.
    let keep_awake = CheckMenuItem::with_id(
        app,
        "keepawake",
        "Keep engine awake",
        true,
        saved.keep_awake,
        None::<&str>,
    )?;

    // The dictionary: the one piece of sayit that needs a real window —
    // rows of text want a text editor, not a menu.
    let dictionary = MenuItem::with_id(app, "dictionary", "Dictionary…", true, None::<&str>)?;

    let about = MenuItem::with_id(
        app,
        "about",
        "made by the Dream Team — GitHub ↗",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit sayit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &mics,
            &dictionary,
            &keep_awake,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &about,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("sayit")
        .icon(
            app.default_window_icon()
                .expect("bundle always has an icon")
                .clone(),
        )
        .tooltip("sayit — warming up")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref();
            if id == "quit" {
                app.exit(0); // RunEvent::Exit kills the sidecar on the way out
            } else if id == "about" {
                // explorer.exe opens URLs in the default browser — no
                // opener plugin needed for one link.
                let _ = std::process::Command::new("explorer")
                    .arg("https://github.com/KhalidAdan/sayit")
                    .spawn();
            } else if id == "dictionary" {
                crate::dictionary::show(app);
            } else if id == "keepawake" {
                // The item toggles itself; report the new state upward, let
                // the coordinator manage its timer, and persist the choice.
                // Persist by load-and-mutate so the untouched fields (the
                // dictionary!) survive the write.
                let on = keep_awake.is_checked().unwrap_or(false);
                println!("[sayit] keep engine awake: {on}");
                let _ = app.emit("keep_awake", on);
                let mut saved = settings::load(app);
                saved.keep_awake = on;
                settings::save(app, &saved);
            } else if id == "autostart" {
                let launcher = app.autolaunch();
                let flip = match launcher.is_enabled() {
                    Ok(true) => launcher.disable(),
                    _ => launcher.enable(),
                };
                if let Err(e) = flip {
                    eprintln!("[sayit] autostart toggle failed: {e}");
                }
                let _ = autostart.set_checked(launcher.is_enabled().unwrap_or(false));
            } else if let Some(name) = id.strip_prefix("mic:") {
                let choice = (!name.is_empty()).then(|| name.to_string());
                println!(
                    "[sayit] microphone: {}",
                    choice.as_deref().unwrap_or("system default")
                );
                *app.state::<MicChoice>().0.lock().unwrap() = choice.clone();
                for item in &mic_items {
                    let _ = item.set_checked(item.id().as_ref() == id);
                }
                let mut saved = settings::load(app);
                saved.microphone = choice;
                settings::save(app, &saved);
            }
        })
        .build(app)?;

    app.state::<Tray>()
        .handles
        .lock()
        .unwrap()
        .replace((tray, status));
    Ok(())
}

pub fn set_status(app: &AppHandle, text: &str) {
    if let Some((tray, status)) = app.state::<Tray>().handles.lock().unwrap().as_ref() {
        let _ = status.set_text(text);
        let _ = tray.set_tooltip(Some(format!("sayit — {text}")));
    }
}
