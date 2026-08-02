//! The platform seam. Every OS-specific behavior in sayit lives behind
//! `trait Platform`, implemented once per OS by a backend struct. This file
//! is the ONLY place a platform `#[cfg]` may appear — CI enforces it with a
//! grep tripwire (exceptions: `#[cfg(test)]` anywhere, main.rs's
//! `windows_subsystem` attribute, and Cargo.toml target tables).
//!
//! Porting sayit to a new OS starts here: write `pub struct NewBackend;`,
//! `impl Platform for NewBackend {}`, and the compiler hands you the
//! complete checklist of what the OS must answer. Items with default
//! bodies are honest no-ops most platforms won't need; everything else is
//! a real seam with a real divergence behind it.

use crate::inject::InjectTiming;
use crate::settings::Settings;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command};
use tauri::{AppHandle, Wry};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub type Current = windows::WindowsBackend;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub type Current = linux::LinuxBackend;

/// The one OS fact deliberately exported to TypeScript: which platform this
/// is and how the user triggers dictation (`src/main.ts` renders the hint
/// in the tray status line).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    pub os: &'static str,
    pub trigger_hint: &'static str,
}

pub trait Platform: Send + Sync + Sized + 'static {
    // ───────── process bootstrap (associated: runs before Tauri exists) ─────────

    /// Environment fixups before GTK/Tauri initializes. Linux forces X11
    /// under GNOME Wayland so the waveform can be positioned; Windows needs
    /// nothing.
    fn pre_launch() {}

    /// A/B self-update rollback state machine, pre-Tauri. Linux restores
    /// the preserved binary when a staged update never reached
    /// `mark_update_healthy`; Windows' installer path needs nothing.
    fn update_preflight() -> Result<(), String> {
        Ok(())
    }

    /// First claim wins: returns false when another sayit already runs.
    /// Windows backs this with a named mutex, claimed before Tauri even
    /// builds; Linux relies on the single-instance plugin and always
    /// claims here.
    fn acquire_single_instance() -> bool {
        true
    }

    /// Construct the backend, starting any long-lived workers. Linux spawns
    /// the uinput + clipboard worker thread here; Windows only reserves the
    /// tray-handle slot.
    fn init() -> Result<Self, String>;

    // ───────── static data (associated) ─────────

    fn info() -> PlatformInfo;

    /// Whether this OS requires the first-run bootstrap window. Existing
    /// installs on platforms that answer `false` must never acquire
    /// onboarding.
    fn needs_setup() -> bool;

    /// The managed per-user data dir, WITHOUT the `SAYIT_DATA_DIR` override
    /// (paths.rs applies that first).
    fn data_dir() -> PathBuf;

    /// The sidecar binary's file name (`whisper-server.exe` vs
    /// `whisper-server`).
    fn sidecar_exe_name() -> &'static str;

    /// Repo/installed-layout search list for `paths::find_from`, as
    /// forward-slash relative paths.
    fn sidecar_candidates() -> &'static [&'static str];

    // ───────── trigger ─────────

    /// Builder-time hook. Windows registers the global-shortcut plugin here
    /// (its trigger is push-driven and must exist before the app runs);
    /// platforms that start their trigger later pass the builder through.
    fn attach(builder: tauri::Builder<Wry>) -> tauri::Builder<Wry> {
        builder
    }

    /// Runtime trigger start, called once setup allows it. Linux spawns the
    /// evdev listeners (idempotent, hotplug-aware); Windows' plugin from
    /// `attach` is already listening, so the default no-op is the honest
    /// answer.
    fn start_trigger(&self, app: &AppHandle) -> Result<(), String> {
        let _ = app;
        Ok(())
    }

    // ───────── injection ─────────

    /// The blocking paste (callers wrap in `spawn_blocking`). Windows:
    /// clipboard save → set → settle → enigo Ctrl+V → restore on a detached
    /// thread. Linux: round-trip to the worker thread that owns the uinput
    /// device and the Wayland clipboard.
    fn inject(&self, text: &str) -> Result<InjectTiming, String>;

    // ───────── sidecar process ─────────

    /// Pre-spawn Command mutation. Windows hides the console window; Linux
    /// ties the child's fate to ours with PDEATHSIG.
    fn configure_sidecar(&self, command: &mut Command);

    /// Post-spawn hook with the live Child. Windows assigns it to the
    /// kill-on-close job object; Linux already made its arrangements in
    /// `configure_sidecar`.
    fn on_sidecar_spawned(&self, child: &Child) {
        let _ = child;
    }

    // ───────── startup policy & setup ─────────

    /// The boot decision. Windows reaps stale engines and starts the
    /// sidecar; Linux gates on `setup_complete` and may show the setup
    /// window instead.
    fn start_normal(&self, app: &AppHandle);

    /// Diagnostics: can we hear the trigger key? (ok, human-readable detail)
    fn keyboard_probe(&self) -> (bool, String);

    /// Diagnostics: can we type into the focused window? (ok, detail)
    fn injection_probe(&self) -> (bool, String);

    /// Diagnostics: where the engine and model live, for display.
    fn engine_model_paths(&self) -> (PathBuf, PathBuf);

    /// First-run bootstrap: probe hardware, fetch companions, warm the
    /// engine. Platforms without a bootstrap return Err — the setup window
    /// never shows there, so the answer is a contract statement, not UX.
    fn setup_begin(&self, app: AppHandle) -> impl Future<Output = Result<(), String>> + Send;

    /// Commit setup: start the trigger, persist `setup_complete`, hide the
    /// window. Platforms without a bootstrap have nothing to commit.
    fn setup_finish(&self, app: &AppHandle) -> Result<(), String> {
        let _ = app;
        Ok(())
    }

    /// The setup window's "test typing" button.
    fn setup_test_injection(&self) -> Result<InjectTiming, String> {
        Err("injection test unavailable on this platform".into())
    }

    // ───────── self-update ─────────

    /// Fire-and-forget: spawn the async update check on the Tauri runtime.
    /// Each backend owns its own spawn — Windows drives the installer
    /// plugin, Linux stages an A/B binary swap.
    fn start_update_check(&self, app: &AppHandle);

    /// Clear the update-pending marker once a boot survived. Linux-only
    /// machinery; the default no-op is correct wherever updates install
    /// atomically.
    fn mark_update_healthy() {}

    // ───────── tray ─────────

    /// Build the tray and stash its handles in the backend. Windows uses
    /// Tauri's native tray; Linux publishes a StatusNotifierItem over D-Bus.
    fn build_tray(&self, app: &AppHandle, saved: &Settings) -> Result<(), String>;

    /// Live status line + tooltip update.
    fn set_tray_status(&self, text: &str);
}
