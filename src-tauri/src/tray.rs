//! The tray: sayit's only permanent visible presence. One status line that
//! says what the key is doing, and a way to quit. Nothing else — a key
//! doesn't have a settings screen, but it does deserve an off switch.
//!
//! The status text is driven from the TypeScript coordinator (which owns
//! the state machine) through the `tray_status` command. Rust renders,
//! TS decides — same seam as everything else.

use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};

#[derive(Default)]
pub struct Tray {
    handles: Mutex<Option<(TrayIcon<Wry>, MenuItem<Wry>)>>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "warming up…", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit sayit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &PredefinedMenuItem::separator(app)?, &quit])?;

    let tray = TrayIconBuilder::with_id("sayit")
        .icon(
            app.default_window_icon()
                .expect("bundle always has an icon")
                .clone(),
        )
        .tooltip("sayit — warming up")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit" {
                app.exit(0); // RunEvent::Exit kills the sidecar on the way out
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
