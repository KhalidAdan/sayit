//! The sidecar's keeper: spawns whisper-server at boot, warms it up, kills
//! it at exit. The first CUDA inference pays a ~55s one-time kernel-init
//! cost (measured — see docs/sidecar.md), so we pay it on half a second of
//! silence at startup. The warmup succeeding doubles as the readiness
//! probe: `sidecar_ready` means "warm and listening".

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::{capture, transcribe};

/// Pullable readiness. The `sidecar_ready` event alone is a race: after
/// the first-ever run the GPU driver caches compiled kernels, warmup
/// finishes in seconds, and the event can fire before the webview has
/// registered its listeners — leaving the coordinator deaf and every
/// press refused. TS asks via `is_ready` at startup AND listens for the
/// event; whichever wins, wins.
#[derive(Default)]
pub struct Ready(pub AtomicBool);

/// v1 runs from this repo on this machine, so both paths are compile-time
/// constants relative to the crate. Bundling for other machines is a future
/// problem (north star: "an installer for strangers").
const SERVER_EXE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    r"\..\sidecar\whisper-cublas\Release\whisper-server.exe"
);
const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), r"\..\models\ggml-small.bin");

pub struct Sidecar(pub Mutex<Option<Child>>);

pub fn spawn(app: AppHandle) -> Result<Child, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let child = Command::new(SERVER_EXE)
        .args([
            "-m",
            MODEL,
            "--host",
            "127.0.0.1",
            "--port",
            &transcribe::SIDECAR_PORT.to_string(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("failed to spawn whisper-server: {e}"))?;

    tauri::async_runtime::spawn(warmup(app));
    Ok(child)
}

async fn warmup(app: AppHandle) {
    let silence = vec![0.0f32; capture::TARGET_SAMPLE_RATE as usize / 2];
    // The server refuses connections until the model is in VRAM (a few
    // seconds), then the first inference itself runs the long CUDA init.
    for attempt in 1..=60u32 {
        match transcribe::transcribe(silence.clone()).await {
            Ok(_) => {
                println!("[sayit] sidecar warm and ready (attempt {attempt})");
                app.state::<Ready>().0.store(true, Ordering::Relaxed);
                let _ = app.emit("sidecar_ready", ());
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    eprintln!("[sayit] sidecar never became ready");
    let _ = app.emit("pipeline_error", "sidecar never became ready");
}
