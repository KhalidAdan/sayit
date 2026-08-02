//! The Windows backend: sayit's birthplace. Global-shortcut plugin for the
//! trigger, enigo Ctrl+V for the paste, a job object so the engine cannot
//! outlive the key, and Tauri's native tray.

mod engine;
mod tray;

use crate::inject::InjectTiming;
use crate::platform::{Platform, PlatformInfo};
use crate::settings::Settings;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri::menu::MenuItem;
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Emitter, Wry};
use tauri_plugin_updater::UpdaterExt;

/// The Windows push-to-talk key.
const PUSH_TO_TALK: &str = "F9";

/// Windows fires auto-repeat Pressed events while the key is held; only the
/// first may start a take.
static HELD: AtomicBool = AtomicBool::new(false);

fn on_shortcut(app: &AppHandle, state: tauri_plugin_global_shortcut::ShortcutState) {
    use tauri_plugin_global_shortcut::ShortcutState;
    match state {
        ShortcutState::Pressed => {
            if !HELD.swap(true, Ordering::SeqCst) {
                let _ = app.emit("push_started", crate::hotkey::now_ms());
            }
        }
        ShortcutState::Released => {
            if HELD.swap(false, Ordering::SeqCst) {
                let _ = app.emit("push_finished", crate::hotkey::now_ms());
            }
        }
    }
}

pub struct WindowsBackend {
    /// (tray icon, status menu item) — populated by `build_tray`.
    tray: Mutex<Option<(TrayIcon<Wry>, MenuItem<Wry>)>>,
}

impl Platform for WindowsBackend {
    /// One sayit per machine. A named mutex is the classic Windows answer;
    /// the handle is deliberately leaked so the claim lasts exactly as long
    /// as the process. This wins the race before Tauri even builds — the
    /// single-instance plugin is the slower cross-platform half.
    fn acquire_single_instance() -> bool {
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let name: Vec<u16> = "Local\\sayit-single-instance\0".encode_utf16().collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            handle.is_null() || GetLastError() != ERROR_ALREADY_EXISTS
        }
    }

    fn init() -> Result<Self, String> {
        Ok(Self {
            tray: Mutex::new(None),
        })
    }

    fn info() -> PlatformInfo {
        PlatformInfo {
            os: "windows",
            trigger_hint: "hold F9 to dictate",
        }
    }

    /// Existing Windows installs must never acquire onboarding.
    fn needs_setup() -> bool {
        false
    }

    fn data_dir() -> PathBuf {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        root.join("sayit")
    }

    fn sidecar_exe_name() -> &'static str {
        "whisper-server.exe"
    }

    fn sidecar_candidates() -> &'static [&'static str] {
        &[
            "sidecar/whisper-server.exe",
            "sidecar/whisper-cublas/Release/whisper-server.exe",
        ]
    }

    fn attach(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
        builder.plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts([PUSH_TO_TALK])
                .expect("push-to-talk key is not a valid shortcut")
                .with_handler(|app, _shortcut, event| on_shortcut(app, event.state()))
                .build(),
        )
    }

    fn inject(&self, text: &str) -> Result<InjectTiming, String> {
        use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

        let t_all = Instant::now();

        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        // Save what the user had. v1 preserves text only; an image on the
        // clipboard is lost to the paste. Known limitation, on the friction list.
        let saved = clipboard.get_text().ok();
        let clipboard_save_ms = t_all.elapsed().as_millis() as u64;

        let t = Instant::now();
        clipboard
            .set_text(text.to_string())
            .map_err(|e| e.to_string())?;
        let clipboard_set_ms = t.elapsed().as_millis() as u64;

        // Windows clipboard updates are not instantaneous; pasting immediately
        // sometimes pastes the old contents. Small settle delay, found empirically.
        let t = Instant::now();
        sleep(Duration::from_millis(60));
        let settle_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let mut enigo = Enigo::new(&EnigoSettings::default()).map_err(|e| e.to_string())?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| e.to_string())?;
        enigo
            .key(Key::Unicode('v'), Direction::Click)
            .map_err(|e| e.to_string())?;
        enigo
            .key(Key::Control, Direction::Release)
            .map_err(|e| e.to_string())?;
        let keystroke_ms = t.elapsed().as_millis() as u64;
        let visible_ms = t_all.elapsed().as_millis() as u64;

        // The receiving app reads the clipboard asynchronously; restoring too
        // soon hands it the old contents instead. 300ms is generous and invisible.
        let t = Instant::now();
        sleep(Duration::from_millis(300));
        if let Some(saved) = saved {
            // Restore on a detached thread: Windows clipboard access can block
            // indefinitely when another process (clipboard managers) holds it.
            // The paste already succeeded — a hung restore must cost at worst
            // the old clipboard contents, never the pipeline.
            std::thread::spawn(move || {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(saved);
                }
            });
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

    fn configure_sidecar(&self, command: &mut Command) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    fn on_sidecar_spawned(&self, child: &Child) {
        engine::assign_to_job(child);
    }

    fn start_normal(&self, app: &AppHandle) {
        // A previous sayit that died uncleanly may have left its engine
        // behind, still holding VRAM and port 8642. Clear it before
        // spawning ours, or we'd run two (docs/sidecar.md).
        engine::reap_stale();
        if let Err(e) = crate::sidecar::start(app) {
            eprintln!("[sayit] engine failed: {e}");
            self.set_tray_status("engine failed — check logs");
        }
    }

    /// The global shortcut either registered at attach() or the app never
    /// built; there is no probe to run. Static answers, preserved verbatim
    /// from the pre-platform setup screen. Honest probing is future work.
    fn keyboard_probe(&self) -> (bool, String) {
        (true, "Windows global shortcut".into())
    }

    fn injection_probe(&self) -> (bool, String) {
        (true, "Windows SendInput".into())
    }

    fn engine_model_paths(&self) -> (PathBuf, PathBuf) {
        (
            crate::paths::sidecar_exe().unwrap_or_default(),
            crate::paths::model().unwrap_or_default(),
        )
    }

    async fn setup_begin(&self, _app: AppHandle) -> Result<(), String> {
        Err("setup bootstrap is only needed on Linux".into())
    }

    fn start_update_check(&self, app: &AppHandle) {
        tauri::async_runtime::spawn(check_and_install(app.clone()));
    }

    fn build_tray(&self, app: &AppHandle, saved: &Settings) -> Result<(), String> {
        tray::build(self, app, saved).map_err(|e| e.to_string())
    }

    fn set_tray_status(&self, text: &str) {
        if let Some((tray, status)) = self.tray.lock().unwrap().as_ref() {
            let _ = status.set_text(text);
            let _ = tray.set_tooltip(Some(format!("sayit — {text}")));
        }
    }
}

/// Tauri's installer-based updater: download, install, takes effect next
/// launch. NSIS handles the swap; no rollback machinery needed on our side.
async fn check_and_install(app: AppHandle) {
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("[update] updater unavailable: {e}");
            return;
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            println!("[update] v{version} available — downloading");
            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => {
                    println!("[update] v{version} staged; takes effect next launch");
                    let _ = app.emit("update_installed", version);
                }
                Err(e) => eprintln!("[update] install failed: {e}"),
            }
        }
        Ok(None) => println!("[update] up to date"),
        Err(e) => eprintln!("[update] check failed (offline is fine): {e}"),
    }
}
