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

use std::sync::Mutex;
use tauri::Manager;

#[tauri::command]
fn start_capture(state: tauri::State<capture::CaptureState>) -> Result<(), String> {
    capture::start(&state)
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
fn play_sound(slot: String, sounds: tauri::State<sounds::Sounds>) {
    println!("[sayit] sound: {slot}");
    sounds.play(&slot);
}

#[tauri::command]
fn tray_status(app: tauri::AppHandle, text: String) {
    tray::set_status(&app, &text);
}

#[tauri::command]
async fn inject_text(text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || inject::inject(&text))
        .await
        .map_err(|e| e.to_string())?
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
        .manage(capture::CaptureState::default())
        .manage(sidecar::Sidecar(Mutex::default()))
        .manage(sounds::start())
        .manage(tray::Tray::default())
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
            tray_status
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
