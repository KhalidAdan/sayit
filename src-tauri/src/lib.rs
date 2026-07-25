//! sayit — the key that listens.
//!
//! Rust owns the four pipeline stages (hotkey, capture, transcribe, inject)
//! because that's where the OS is. The TypeScript side owns the state
//! machine and decides what happens next. Rust touches the OS, TS makes
//! decisions, the sidecar thinks.

mod capture;
mod hotkey;
mod inject;
mod sidecar;
mod sounds;
mod transcribe;
mod tray;

use std::io::Write;
use std::sync::Mutex;
use tauri::Manager;

/// The tray's microphone pick. None = system default.
pub struct MicChoice(pub Mutex<Option<String>>);

/// Where the gap dataset accumulates: one CSV row per successful take.
/// Gitignored — it's this machine's diary, not source code.
const GAP_LOG: &str = concat!(env!("CARGO_MANIFEST_DIR"), r"\..\gap-log.csv");

#[tauri::command]
fn start_capture(
    app: tauri::AppHandle,
    state: tauri::State<capture::CaptureState>,
    mic: tauri::State<MicChoice>,
) -> Result<(), String> {
    let preferred = mic.0.lock().unwrap().clone();
    capture::start(&state, &app, preferred)
}

#[tauri::command]
async fn stop_and_transcribe(
    state: tauri::State<'_, capture::CaptureState>,
) -> Result<String, String> {
    let samples = capture::stop(&state)?;
    println!(
        "[sayit] captured {:.1}s of audio",
        samples.len() as f32 / capture::TARGET_SAMPLE_RATE as f32
    );
    let text = transcribe::transcribe(samples).await?;
    println!("[sayit] transcribed: {text:?}");
    Ok(text)
}

#[tauri::command]
async fn inject_text(text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || inject::inject(&text))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn play_sound(slot: String, sounds: tauri::State<sounds::Sounds>) {
    println!("[sayit] sound: {slot}");
    sounds.play(&slot);
}

#[tauri::command]
fn tray_status(app: tauri::AppHandle, text: String) {
    tray::set_status(&app, &text);
}

#[tauri::command]
fn is_ready(ready: tauri::State<sidecar::Ready>) -> bool {
    ready.0.load(std::sync::atomic::Ordering::Relaxed)
}

/// The gap, measured, not vibed: release-to-text-landed per take. Printed
/// for the log and appended to the CSV so v3's entry gate has a dataset.
#[tauri::command]
fn log_gap(total_ms: u64, chars: usize) {
    println!("[sayit] gap: {total_ms}ms for {chars} chars");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let new = !std::path::Path::new(GAP_LOG).exists();
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(GAP_LOG) {
        if new {
            let _ = writeln!(file, "unix_ts,total_ms,chars");
        }
        let _ = writeln!(file, "{stamp},{total_ms},{chars}");
    }
}

/// The waveform window appears bottom-center while recording. It is
/// focusable:false in config — it must NEVER take focus, or the paste
/// lands in the wrong place. Show/hide live on the Rust side so the
/// webview needs no window-management capabilities.
#[tauri::command]
fn waveform_show(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("waveform") {
        if let (Ok(Some(monitor)), Ok(size)) = (w.primary_monitor(), w.outer_size()) {
            let screen = monitor.size();
            let x = screen.width.saturating_sub(size.width) / 2;
            let y = screen.height.saturating_sub(size.height + 96);
            let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        }
        let _ = w.show();
    }
}

#[tauri::command]
fn waveform_hide(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("waveform") {
        let _ = w.hide();
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts([hotkey::PUSH_TO_TALK])
                .expect("push-to-talk key is not a valid shortcut")
                .with_handler(|app, _shortcut, event| hotkey::on_shortcut(app, event.state()))
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(capture::CaptureState::default())
        .manage(sidecar::Sidecar(Mutex::default()))
        .manage(sidecar::Ready::default())
        .manage(sounds::start())
        .manage(tray::Tray::default())
        .manage(MicChoice(Mutex::new(None)))
        .setup(|app| {
            tray::build(app.handle())?;
            let child = sidecar::spawn(app.handle().clone())?;
            *app.state::<sidecar::Sidecar>().0.lock().unwrap() = Some(child);
            println!("[sayit] push-to-talk on {}", hotkey::PUSH_TO_TALK);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_capture,
            stop_and_transcribe,
            inject_text,
            play_sound,
            tray_status,
            is_ready,
            log_gap,
            waveform_show,
            waveform_hide
        ])
        .build(tauri::generate_context!())
        .expect("error building sayit")
        .run(|app, event| {
            // The sidecar is our child; if we exit and leave it running, it
            // squats on the port and half a gig of VRAM forever.
            if let tauri::RunEvent::Exit = event {
                if let Some(mut child) = app.state::<sidecar::Sidecar>().0.lock().unwrap().take() {
                    let _ = child.kill();
                }
            }
        });
}
