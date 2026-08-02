//! The engine's keeper: whisper-server's full lifecycle. Started at boot,
//! put to sleep when the coordinator says so (idle timer), woken on
//! demand, killed at exit — and, via the platform backend (job object on
//! Windows, PDEATHSIG on Linux), killed by the OS itself if sayit dies
//! any less politely. Sleeping frees ~500MB of
//! VRAM; waking costs seconds (the GPU driver caches compiled kernels
//! after the first-ever run). The user never manages any of this — the key
//! always works, and the tray narrates.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::platform::Platform;
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
    let sidecar = app.state::<Sidecar>();
    let mut guard = sidecar.0.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }

    // Companions are found at runtime (env override, next-to-exe, or repo
    // layout) — see paths.rs. The same exe works everywhere.
    let server = crate::paths::sidecar_exe()?;
    let model = crate::paths::model()?;
    let mut command = Command::new(server);
    command
        .arg("-m")
        .arg(&model)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(transcribe::SIDECAR_PORT.to_string());
    let platform = app.state::<std::sync::Arc<crate::platform::Current>>();
    platform.configure_sidecar(&mut command);
    let child = command
        .spawn()
        .map_err(|e| format!("failed to spawn whisper-server: {e}"))?;
    // The platform ties the child's fate to ours (job object on Windows;
    // Linux already arranged PDEATHSIG in configure_sidecar).
    platform.on_sidecar_spawned(&child);
    *guard = Some(child);
    drop(guard);

    println!("[sayit] engine waking");
    let _ = app.emit("engine_waking", ());
    tauri::async_runtime::spawn(warmup(app.clone(), std::time::Instant::now()));
    Ok(())
}

/// Put the engine to sleep: kill the process, free the VRAM. Only the
/// coordinator calls this, and only from idle.
pub fn sleep(app: &AppHandle) {
    if let Some(mut child) = app.state::<Sidecar>().0.lock().unwrap().take() {
        let _ = child.kill();
        // Reap the process object so "VRAM freed" below is true, not hopeful.
        let _ = child.wait();
        app.state::<Ready>().0.store(false, Ordering::Relaxed);
        println!("[sayit] engine sleeping — VRAM freed");
        let _ = app.emit("engine_sleeping", ());
    }
}

pub fn stop(app: &AppHandle) {
    if let Some(mut child) = app.state::<Sidecar>().0.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    app.state::<Ready>().0.store(false, Ordering::Relaxed);
}

/// Warm the engine with half a second of silence. At first-ever boot this
/// pays the ~55s CUDA kernel-init cost (measured; docs/sidecar.md) so the
/// user never does; on later wakes it's seconds. Success doubles as the
/// readiness probe: `sidecar_ready` means "warm and listening".
async fn warmup(app: AppHandle, spawned: std::time::Instant) {
    let silence = vec![0.0f32; capture::TARGET_SAMPLE_RATE as usize / 2];
    for probe in 1..=60u32 {
        // The engine may have been put back to sleep mid-warmup; stop probing.
        if app.state::<Sidecar>().0.lock().unwrap().is_none() {
            return;
        }
        match transcribe::transcribe(silence.clone()).await {
            Ok(_) => {
                println!(
                    "[timing] engine warm and ready in {:.1}s ({probe} probe{})",
                    spawned.elapsed().as_secs_f32(),
                    if probe == 1 { "" } else { "s" }
                );
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
