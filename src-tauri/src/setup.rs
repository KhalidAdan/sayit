//! First-run bootstrap and reusable diagnostics — the OS-free half. The
//! setup window's commands live here as thin shims over the platform
//! backend; what the probes actually check (evdev, uinput, downloads) is
//! the backend's business. The first failure gets a focused window; after
//! completion, failures stay in the tray unless the user explicitly opens
//! Diagnostics.

use crate::platform::{self, Platform};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

#[derive(Default)]
pub struct SetupState {
    pub(crate) running: AtomicBool,
    pub(crate) ready: AtomicBool,
    pub(crate) error: Mutex<Option<String>>,
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

pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn backend(app: &AppHandle) -> Arc<platform::Current> {
    app.state::<Arc<platform::Current>>().inner().clone()
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
    let platform = backend(&app);
    let (keyboard_ok, keyboard_detail) = platform.keyboard_probe();
    let (uinput_ok, uinput_detail) = platform.injection_probe();

    let microphones = crate::capture::list_inputs();
    let microphone_ok = !microphones.is_empty();
    let microphone_detail = if microphone_ok {
        microphones.join(", ")
    } else {
        "no microphone found".into()
    };

    let (engine, model) = platform.engine_model_paths();

    let last_error = state.error.lock().unwrap().clone();
    Snapshot {
        first_run: platform::Current::needs_setup() && !crate::settings::load(&app).setup_complete,
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
    let platform = backend(&app);
    platform.setup_begin(app).await
}

#[tauri::command]
pub fn setup_finish(app: AppHandle) -> Result<(), String> {
    backend(&app).setup_finish(&app)
}

#[tauri::command]
pub async fn setup_test_injection(app: AppHandle) -> Result<crate::inject::InjectTiming, String> {
    let platform = backend(&app);
    tauri::async_runtime::spawn_blocking(move || platform.setup_test_injection())
        .await
        .map_err(|e| e.to_string())?
}
