//! The engine's keeper: whisper-server's full lifecycle. Started at boot,
//! put to sleep when the coordinator says so (idle timer), woken on
//! demand, killed at exit — and, via the platform backend (job object on
//! Windows, PDEATHSIG on Linux), killed by the OS itself if sayit dies
//! any less politely. Sleeping frees ~500MB of
//! VRAM; waking costs seconds (the GPU driver caches compiled kernels
//! after the first-ever run). The user never manages any of this — the key
//! always works, and the tray narrates.

use std::path::PathBuf;
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
    add_vad(&mut command, crate::settings::load(app).vad);
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

// Silence filter, layer 2 — OPT-IN: whisper-server's built-in voice
// activity detector. Silero VAD (a 0.9 MB model the server runs on the
// CPU) scores each 32 ms slice of the take for "is this speech?"; whisper
// then hears only the stretches that are. A take of room tone, hum, a
// breath or key clicks comes back "" instead of the "Thank you." whisper
// invents for non-speech. Layer 1 (silence.rs) already refused the quiet
// takes; these are the loud ones it can't judge.
//
// Off unless settings.json says `"vad": true`, because it widens the gap
// on every spoken take: +76ms on a 6.7s take, +421ms on a 20s one
// (RTX 2070, interleaved medians), and its benefit on a real mic is still
// unproven. The likely future is per-request VAD only for takes the gate
// can't judge. The three knobs below were tuned on synthetic clips — TTS
// speech at normal and very quiet levels, silence, hiss, rumble, hum, a
// breath, key clicks (numbers in the PR).

/// Speech probability (0–1) a slice needs to count as speech. whisper's
/// default 0.5 dropped a lone, very quiet "yes" entirely; 0.4 — the value
/// hex ships — kept it, and still called every noise clip empty. Lower
/// favors quiet speech; keeping noise out is the other layers' job too.
const VAD_THRESHOLD: &str = "0.4";
/// Speech shorter than this is ignored. The default 250 ms risks a clipped
/// one-syllable word; 160 ms is hex's floor, and layer 1's 180 ms run
/// rule is built on it.
const VAD_MIN_SPEECH_MS: &str = "160";
/// Each detected stretch is widened by this much on both sides before
/// whisper hears it. The default 30 ms cut "The quick" off the front of a
/// quiet sentence; 300 ms brought it back. Padding adds audio, not encoder
/// work — whisper's encoder pads every clip to a 30-second window anyway —
/// though the pad's own share of the VAD's cost wasn't isolated.
const VAD_SPEECH_PAD_MS: &str = "300";

/// The engine starts at boot and on every wake from sleep; say which way
/// the VAD went the first time only. (Like idle_minutes: edit
/// settings.json, then restart sayit.)
static VAD_STATUS_LOGGED: std::sync::Once = std::sync::Once::new();

/// Should the engine get the VAD flags, and with which file? Only when
/// BOTH hold: settings.json opted in, and paths.rs found a whole model.
/// Ok(path) = pass the flags; Err(why) = run exactly as before, and `why`
/// is the one log line saying so. `find_model` is only called once the
/// setting says yes — an opted-out sayit never even looks for the file.
/// Pure (the search is passed in), so the tests can ask it directly.
fn vad_choice(
    wanted: bool,
    find_model: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    if !wanted {
        return Err("off by default (opt in: \"vad\": true in settings.json)".into());
    }
    find_model().map_err(|why| format!("opted in, but {why}"))
}

/// Turn on the VAD when `vad_choice` says so; otherwise leave the engine's
/// command line exactly as it always was.
fn add_vad(command: &mut Command, wanted: bool) {
    let vad = vad_choice(wanted, crate::paths::vad_model);
    VAD_STATUS_LOGGED.call_once(|| match &vad {
        Ok(path) => println!("[sayit] engine VAD on: {}", path.display()),
        Err(why) => println!("[sayit] engine VAD off — {why}"),
    });
    if let Ok(path) = vad {
        command
            .arg("--vad")
            .arg("--vad-model")
            .arg(path)
            .arg("--vad-threshold")
            .arg(VAD_THRESHOLD)
            .arg("--vad-min-speech-duration-ms")
            .arg(VAD_MIN_SPEECH_MS)
            .arg("--vad-speech-pad-ms")
            .arg(VAD_SPEECH_PAD_MS);
    }
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
        match warm_once(&silence).await {
            Ok(()) => {
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

/// One warmup request: the silence, with the VAD switched off for this
/// request only (`vad=false`; whisper-server honors it per request). With
/// the VAD on, silence never reaches whisper's encoder — so a silent
/// warmup would warm nothing, and the first real take after every wake
/// would pay the CUDA start-up warmup exists to hide. Measured while
/// building this: a VAD-on silent warmup left the first take ~400–600ms
/// slower than the second; with `vad=false` the first take ran at the
/// steady-state speed. On an engine started without VAD the field is a
/// no-op.
async fn warm_once(silence: &[f32]) -> Result<(), String> {
    let part = reqwest::multipart::Part::bytes(transcribe::wav_bytes(silence)?)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json")
        .text("vad", "false");
    reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/inference",
            transcribe::SIDECAR_PORT
        ))
        .multipart(form)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found() -> Result<PathBuf, String> {
        Ok(PathBuf::from("models/ggml-silero-v5.1.2.bin"))
    }

    fn missing() -> Result<PathBuf, String> {
        Err("ggml-silero-v5.1.2.bin not found".into())
    }

    #[test]
    fn setting_off_means_no_vad_even_with_the_file_present() {
        // The default: the gap stays as it was, file or no file.
        let why = vad_choice(false, found).unwrap_err();
        assert!(why.contains("\"vad\": true"), "says how to opt in: {why}");
        // ...and an opted-out sayit doesn't even go looking.
        assert!(vad_choice(false, || panic!("searched while opted out")).is_err());
    }

    #[test]
    fn opted_in_without_the_file_runs_without_vad_and_says_why() {
        let why = vad_choice(true, missing).unwrap_err();
        assert!(why.starts_with("opted in, but"), "{why}");
        assert!(why.contains("not found"), "{why}");
    }

    #[test]
    fn opted_in_with_the_file_turns_it_on() {
        assert_eq!(vad_choice(true, found), found());
    }
}
