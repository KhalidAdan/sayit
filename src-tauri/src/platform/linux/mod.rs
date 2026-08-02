//! The Linux backend: evdev in, uinput out, ksni tray, XDG paths, a
//! first-run bootstrap that downloads the engine, and an A/B self-update
//! with rollback. Everything the immutable-distro host needs and nothing
//! the Windows binary has to carry.

mod assets;
mod hotkey;
mod input;
mod tray;
mod update;

use crate::inject::InjectTiming;
use crate::platform::{Platform, PlatformInfo};
use crate::settings::Settings;
use crate::setup::SetupState;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

pub struct LinuxBackend {
    /// Channel to the worker thread owning the uinput device and the
    /// long-lived Wayland clipboard.
    input: input::LinuxInput,
    /// The evdev listener may be started from start_normal OR setup_finish;
    /// exactly one may win.
    trigger_started: AtomicBool,
    /// ksni handle — populated by `build_tray`.
    tray: Mutex<Option<ksni::blocking::Handle<tray::SayitTray>>>,
}

impl Platform for LinuxBackend {
    fn pre_launch() {
        // GNOME Wayland intentionally forbids top-level positioning. sayit's
        // non-focusable waveform has a physical place (bottom centre), so the
        // tiny Tauri UI runs through XWayland while input/audio remain native.
        if std::env::var_os("DISPLAY").is_some()
            && std::env::var_os("SAYIT_NATIVE_WAYLAND").is_none()
        {
            std::env::set_var("GDK_BACKEND", "x11");
            // WebKitGTK's DMA-BUF path is unreliable through NVIDIA XWayland
            // (failed GBM allocations render an empty setup window).
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    fn update_preflight() -> Result<(), String> {
        update::preflight()
    }

    fn init() -> Result<Self, String> {
        Ok(Self {
            input: input::LinuxInput::start(),
            trigger_started: AtomicBool::new(false),
            tray: Mutex::new(None),
        })
    }

    fn info() -> PlatformInfo {
        PlatformInfo {
            os: "linux",
            trigger_hint: "double-tap left Ctrl or remapped Caps to dictate",
        }
    }

    fn needs_setup() -> bool {
        true
    }

    fn data_dir() -> PathBuf {
        let root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("dev.khalid.sayit")
    }

    fn sidecar_exe_name() -> &'static str {
        "whisper-server"
    }

    fn sidecar_candidates() -> &'static [&'static str] {
        &[
            "sidecar/whisper-server",
            "sidecar/whisper-cuda/whisper-server",
            "sidecar/whisper-bin-x64/whisper-server",
        ]
    }

    fn start_trigger(&self, app: &AppHandle) -> Result<(), String> {
        if self.trigger_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        match hotkey::start(app) {
            Ok(count) => {
                println!("[sayit] Control gesture listening on {count} keyboard device(s)");
                Ok(())
            }
            Err(e) => {
                self.trigger_started.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    fn inject(&self, text: &str) -> Result<InjectTiming, String> {
        self.input.inject(text.to_owned())
    }

    fn configure_sidecar(&self, command: &mut Command) {
        use std::os::unix::process::CommandExt;
        // A crashed parent must not leave a server owning the port and VRAM.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    fn start_normal(&self, app: &AppHandle) {
        let saved = crate::settings::load(app);
        if !saved.setup_complete {
            self.set_tray_status("setup required — open sayit");
            crate::setup::show(app);
            return;
        }
        if let Err(e) = self.start_trigger(app) {
            *app.state::<SetupState>().error.lock().unwrap() = Some(e.clone());
            self.set_tray_status("keyboard unavailable — Diagnostics…");
            return;
        }
        if !assets::assets_ready() {
            *app.state::<SetupState>().error.lock().unwrap() =
                Some("engine or model is missing".into());
            self.set_tray_status("engine missing — Diagnostics…");
            return;
        }
        if let Err(e) = crate::sidecar::start(app) {
            *app.state::<SetupState>().error.lock().unwrap() = Some(e.clone());
            self.set_tray_status("engine failed — Diagnostics…");
        }
    }

    fn keyboard_probe(&self) -> (bool, String) {
        match hotkey::probe() {
            Ok(devices) => (true, devices.join(", ")),
            Err(e) => (false, e),
        }
    }

    fn injection_probe(&self) -> (bool, String) {
        match input::probe_uinput() {
            Ok(()) => (true, "/dev/uinput is writable".into()),
            Err(e) => (false, e),
        }
    }

    fn engine_model_paths(&self) -> (PathBuf, PathBuf) {
        assets::paths()
    }

    async fn setup_begin(&self, app: AppHandle) -> Result<(), String> {
        let state = app.state::<SetupState>();
        if state.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        state.ready.store(false, Ordering::Relaxed);
        *state.error.lock().unwrap() = None;

        let result: Result<(), String> = async {
            hotkey::probe()?;
            input::probe_uinput()?;
            if crate::capture::list_inputs().is_empty() {
                return Err("no microphone found".into());
            }
            assets::ensure(&app).await?;
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

    fn setup_finish(&self, app: &AppHandle) -> Result<(), String> {
        let state = app.state::<SetupState>();
        if !state.ready.load(Ordering::Relaxed)
            && !(assets::assets_ready()
                && app
                    .state::<crate::sidecar::Ready>()
                    .0
                    .load(Ordering::Relaxed))
        {
            return Err("setup is not ready yet".into());
        }
        self.start_trigger(app)?;
        let mut saved = crate::settings::load(app);
        saved.setup_complete = true;
        crate::settings::save(app, &saved);
        self.set_tray_status("ready — double-tap left Ctrl or remapped Caps");
        if let Some(window) = app.get_webview_window("setup") {
            let _ = window.hide();
        }
        let _ = app.emit("setup_finished", ());
        Ok(())
    }

    fn setup_test_injection(&self) -> Result<InjectTiming, String> {
        self.input.inject("sayit can type here".to_owned())
    }

    fn start_update_check(&self, app: &AppHandle) {
        tauri::async_runtime::spawn(update::check_and_install(app.clone()));
    }

    fn mark_update_healthy() {
        update::mark_healthy();
    }

    fn build_tray(&self, app: &AppHandle, saved: &Settings) -> Result<(), String> {
        tray::build(self, app, saved)
    }

    fn set_tray_status(&self, text: &str) {
        if let Some(handle) = self.tray.lock().unwrap().as_ref() {
            let text = text.to_owned();
            let _ = handle.update(move |tray| tray.status = text);
        }
    }
}
