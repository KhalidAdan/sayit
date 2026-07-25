//! The engine's keeper: whisper-server's full lifecycle. Started at boot,
//! put to sleep when the coordinator says so (idle timer), woken on
//! demand, killed at exit. Sleeping frees ~500MB of VRAM; waking costs
//! seconds (the GPU driver caches compiled kernels after the first-ever
//! run). The user never manages any of this — the key always works, and
//! the tray narrates.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::{capture, transcribe};

/// Pullable readiness. The `sidecar_ready` event alone is a race: warmup
/// can finish before the webview has registered its listeners — leaving
/// the coordinator deaf. TS asks via `is_ready` at startup AND listens
/// for the event; whichever wins, wins.
#[derive(Default)]
pub struct Ready(pub AtomicBool);

pub struct Sidecar(pub Mutex<Option<Child>>);

/// Start (or wake) the engine. Idempotent: a running or already-waking
/// engine is left alone, so the coordinator can call this on every press
/// without thinking.
pub fn start(app: &AppHandle) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let sidecar = app.state::<Sidecar>();
    let mut guard = sidecar.0.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }

    // Companions are found at runtime (env override, next-to-exe, or repo
    // layout) — see paths.rs. The same exe works everywhere.
    let server = crate::paths::sidecar_exe()?;
    let model = crate::paths::model()?;
    let child = Command::new(server)
        .arg("-m")
        .arg(&model)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(transcribe::SIDECAR_PORT.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("failed to spawn whisper-server: {e}"))?;
    *guard = Some(child);
    drop(guard);

    println!("[sayit] engine waking");
    let _ = app.emit("engine_waking", ());
    tauri::async_runtime::spawn(warmup(app.clone()));
    Ok(())
}

/// Put the engine to sleep: kill the process, free the VRAM. Only the
/// coordinator calls this, and only from idle.
pub fn sleep(app: &AppHandle) {
    if let Some(mut child) = app.state::<Sidecar>().0.lock().unwrap().take() {
        let _ = child.kill();
        app.state::<Ready>().0.store(false, Ordering::Relaxed);
        println!("[sayit] engine sleeping — VRAM freed");
        let _ = app.emit("engine_sleeping", ());
    }
}

/// Warm the engine with half a second of silence. At first-ever boot this
/// pays the ~55s CUDA kernel-init cost (measured; docs/sidecar.md) so the
/// user never does; on later wakes it's seconds. Success doubles as the
/// readiness probe: `sidecar_ready` means "warm and listening".
async fn warmup(app: AppHandle) {
    let silence = vec![0.0f32; capture::TARGET_SAMPLE_RATE as usize / 2];
    for _ in 1..=60u32 {
        // The engine may have been put back to sleep mid-warmup; stop probing.
        if app.state::<Sidecar>().0.lock().unwrap().is_none() {
            return;
        }
        match transcribe::transcribe(silence.clone()).await {
            Ok(_) => {
                println!("[sayit] engine warm and ready");
                app.state::<Ready>().0.store(true, Ordering::Relaxed);
                let _ = app.emit("sidecar_ready", ());
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    eprintln!("[sayit] engine never became ready");
    let _ = app.emit("pipeline_error", "engine never became ready");
}
