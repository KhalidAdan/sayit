//! Linux self-update: download the signed raw binary, probe it, and
//! atomically swap it in place; the old inode keeps the running process
//! alive until the next normal launch. preflight() is the boot-time
//! rollback state machine for a candidate that never proved healthy.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let name = exe.file_name().unwrap_or_default().to_string_lossy();
    exe.with_file_name(format!(".{name}.{suffix}"))
}

fn stage(bytes: &[u8]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let next = sibling(&exe, "new");
    let previous = sibling(&exe, "previous");
    let marker = sibling(&exe, "update-pending");

    let _ = fs::remove_file(&next);
    let mut file = File::create(&next)
        .map_err(|e| format!("cannot write update beside {}: {e}", exe.display()))?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let mut permissions = file.metadata().map_err(|e| e.to_string())?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&next, permissions).map_err(|e| e.to_string())?;

    let probe = Command::new(&next)
        .arg("--update-probe")
        .status()
        .map_err(|e| format!("updated binary could not start: {e}"))?;
    if !probe.success() {
        let _ = fs::remove_file(&next);
        return Err(format!("updated binary probe exited with {probe}"));
    }

    let _ = fs::remove_file(&previous);
    fs::rename(&exe, &previous).map_err(|e| e.to_string())?;
    if let Err(e) = fs::rename(&next, &exe) {
        let _ = fs::rename(&previous, &exe);
        return Err(e.to_string());
    }
    fs::write(marker, b"staged\n").map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) async fn check_and_install(app: AppHandle) {
    // Development binaries must never overwrite themselves with a release.
    if cfg!(debug_assertions) {
        return;
    }
    let updater = match app.updater_builder().target("linux-x86_64-bin").build() {
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
            match update.download(|_, _| {}, || {}).await {
                Ok(bytes) => match stage(&bytes) {
                    Ok(()) => {
                        println!("[update] v{version} staged; takes effect next launch");
                        let _ = app.emit("update_installed", version);
                    }
                    Err(e) => eprintln!("[update] install failed: {e}"),
                },
                Err(e) => eprintln!("[update] download failed: {e}"),
            }
        }
        Ok(None) => println!("[update] up to date"),
        Err(e) => eprintln!("[update] check failed (offline is fine): {e}"),
    }
}

/// Called before GTK/Tauri starts. A second attempt to boot a candidate
/// that never reached mark_healthy restores the preserved executable.
pub(super) fn preflight() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let previous = sibling(&exe, "previous");
    let marker = sibling(&exe, "update-pending");
    let Ok(state) = fs::read_to_string(&marker) else {
        return Ok(());
    };
    if state.trim() == "staged" {
        fs::write(marker, b"testing\n").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if state.trim() == "testing" && previous.exists() {
        let failed = sibling(&exe, "failed");
        let _ = fs::remove_file(&failed);
        fs::rename(&exe, &failed).map_err(|e| e.to_string())?;
        fs::rename(&previous, &exe).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&marker);
        Command::new(&exe)
            .spawn()
            .map_err(|e| format!("rollback restored but could not start: {e}"))?;
        std::process::exit(0);
    }
    Ok(())
}

pub(super) fn mark_healthy() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let marker = sibling(&exe, "update-pending");
    if marker.exists() {
        let _ = fs::remove_file(marker);
        let _ = fs::remove_file(sibling(&exe, "previous"));
    }
}
