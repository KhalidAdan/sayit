//! Linux first-run bootstrap and reusable diagnostics. The first failure gets
//! a focused window; after completion, failures stay in the tray unless the
//! user explicitly opens Diagnostics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
pub struct SetupState {
    running: AtomicBool,
    ready: AtomicBool,
    hotkey_started: AtomicBool,
    error: Mutex<Option<String>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    first_run: bool,
    running: bool,
    ready: bool,
    keyboard_ok: bool,
    keyboard_detail: String,
    uinput_ok: bool,
    uinput_detail: String,
    microphone_ok: bool,
    microphone_detail: String,
    assets_ok: bool,
    engine_path: String,
    model_path: String,
    error: Option<String>,
}

pub fn init_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let copy = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = copy.hide();
            }
        });
    }
}

#[cfg(target_os = "linux")]
fn start_hotkey_once(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<SetupState>();
    if state.hotkey_started.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    match crate::hotkey::start_linux(app) {
        Ok(count) => {
            println!("[sayit] Control gesture listening on {count} keyboard device(s)");
            Ok(())
        }
        Err(e) => {
            state.hotkey_started.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

#[cfg(target_os = "linux")]
pub fn start_normal(app: &AppHandle) {
    let saved = crate::settings::load(app);
    if !saved.setup_complete {
        crate::tray::set_status(app, "setup required — open sayit");
        show(app);
        return;
    }
    if let Err(e) = start_hotkey_once(app) {
        *app.state::<SetupState>().error.lock().unwrap() = Some(e.clone());
        crate::tray::set_status(app, "keyboard unavailable — Diagnostics…");
        return;
    }
    if !crate::assets::assets_ready() {
        *app.state::<SetupState>().error.lock().unwrap() =
            Some("engine or model is missing".into());
        crate::tray::set_status(app, "engine missing — Diagnostics…");
        return;
    }
    if let Err(e) = crate::sidecar::start(app) {
        *app.state::<SetupState>().error.lock().unwrap() = Some(e.clone());
        crate::tray::set_status(app, "engine failed — Diagnostics…");
    }
}

#[cfg(windows)]
pub fn start_normal(app: &AppHandle) {
    if let Err(e) = crate::sidecar::start(app) {
        eprintln!("[sayit] engine failed: {e}");
        crate::tray::set_status(app, "engine failed — check logs");
    }
}

pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub fn setup_show(app: AppHandle) {
    show(&app);
}

#[tauri::command]
pub fn setup_hide(app: AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.hide();
    }
}

#[tauri::command]
pub fn setup_snapshot(app: AppHandle) -> Snapshot {
    let state = app.state::<SetupState>();
    #[cfg(target_os = "linux")]
    let (keyboard_ok, keyboard_detail) = match crate::hotkey::probe_linux() {
        Ok(devices) => (true, devices.join(", ")),
        Err(e) => (false, e),
    };
    #[cfg(not(target_os = "linux"))]
    let (keyboard_ok, keyboard_detail) = (true, "Windows global shortcut".into());

    #[cfg(target_os = "linux")]
    let (uinput_ok, uinput_detail) = match crate::linux_input::probe_uinput() {
        Ok(()) => (true, "/dev/uinput is writable".into()),
        Err(e) => (false, e),
    };
    #[cfg(not(target_os = "linux"))]
    let (uinput_ok, uinput_detail) = (true, "Windows SendInput".into());

    let microphones = crate::capture::list_inputs();
    let microphone_ok = !microphones.is_empty();
    let microphone_detail = if microphone_ok {
        microphones.join(", ")
    } else {
        "no microphone found".into()
    };

    #[cfg(target_os = "linux")]
    let (engine, model) = crate::assets::paths();
    #[cfg(not(target_os = "linux"))]
    let (engine, model) = (
        crate::paths::sidecar_exe().unwrap_or_default(),
        crate::paths::model().unwrap_or_default(),
    );

    let last_error = state.error.lock().unwrap().clone();
    Snapshot {
        first_run: cfg!(target_os = "linux") && !crate::settings::load(&app).setup_complete,
        running: state.running.load(Ordering::Relaxed),
        ready: state.ready.load(Ordering::Relaxed),
        keyboard_ok,
        keyboard_detail,
        uinput_ok,
        uinput_detail,
        microphone_ok,
        microphone_detail,
        assets_ok: crate::paths::sidecar_exe().is_ok() && crate::paths::model().is_ok(),
        engine_path: engine.display().to_string(),
        model_path: model.display().to_string(),
        error: last_error,
    }
}

#[tauri::command]
pub async fn setup_begin(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    return Err("setup bootstrap is only needed on Linux".into());

    #[cfg(target_os = "linux")]
    {
        let state = app.state::<SetupState>();
        if state.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        state.ready.store(false, Ordering::Relaxed);
        *state.error.lock().unwrap() = None;

        let result: Result<(), String> = async {
            crate::hotkey::probe_linux()?;
            crate::linux_input::probe_uinput()?;
            if crate::capture::list_inputs().is_empty() {
                return Err("no microphone found".into());
            }
            crate::assets::ensure(&app).await?;
            crate::sidecar::start(&app)?;

            for _ in 0..240 {
                if app
                    .state::<crate::sidecar::Ready>()
                    .0
                    .load(Ordering::Relaxed)
                {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err("engine warmup timed out".into())
        }
        .await;

        state.running.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => {
                state.ready.store(true, Ordering::Relaxed);
                let _ = app.emit("setup_ready", ());
                Ok(())
            }
            Err(e) => {
                *state.error.lock().unwrap() = Some(e.clone());
                let _ = app.emit("setup_failed", e.clone());
                Err(e)
            }
        }
    }
}

#[tauri::command]
pub fn setup_finish(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let state = app.state::<SetupState>();
        if !state.ready.load(Ordering::Relaxed)
            && !(crate::assets::assets_ready()
                && app
                    .state::<crate::sidecar::Ready>()
                    .0
                    .load(Ordering::Relaxed))
        {
            return Err("setup is not ready yet".into());
        }
        start_hotkey_once(&app)?;
        let mut saved = crate::settings::load(&app);
        saved.setup_complete = true;
        crate::settings::save(&app, &saved);
        crate::tray::set_status(&app, "ready — double-tap left Ctrl or remapped Caps");
        if let Some(window) = app.get_webview_window("setup") {
            let _ = window.hide();
        }
        let _ = app.emit("setup_finished", ());
        return Ok(());
    }
    #[cfg(not(target_os = "linux"))]
    Ok(())
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn setup_test_injection(
    input: tauri::State<'_, crate::linux_input::LinuxInput>,
) -> Result<crate::inject::InjectTiming, String> {
    let input = input.inner().clone();
    tauri::async_runtime::spawn_blocking(move || input.inject("sayit can type here".to_owned()))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub async fn setup_test_injection() -> Result<crate::inject::InjectTiming, String> {
    Err("Linux injection test unavailable".into())
}
