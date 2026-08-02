//! sayit — the key that listens.
//!
//! Rust owns the four pipeline stages (hotkey, capture, transcribe, inject)
//! because that's where the OS is. The TypeScript side owns the state
//! machine and decides what happens next. Rust touches the OS, TS makes
//! decisions, the sidecar thinks.

mod assets;
mod capture;
mod dictionary;
mod hotkey;
mod inject;
#[cfg(target_os = "linux")]
mod linux_input;
mod paths;
mod settings;
mod setup;
mod sidecar;
mod sounds;
mod transcribe;
mod tray;
mod update;

use std::io::Write;
use std::sync::Mutex;
use tauri::Manager;

/// The tray's microphone pick. None = system default.
pub struct MicChoice(pub Mutex<Option<String>>);

#[tauri::command]
fn start_capture(
    app: tauri::AppHandle,
    state: tauri::State<capture::CaptureState>,
    mic: tauri::State<MicChoice>,
) -> Result<(), String> {
    let preferred = mic.0.lock().unwrap().clone();
    capture::start(&state, &app, preferred)
}

/// One take's text plus where every one of its milliseconds went. The
/// coordinator merges this with its own clocks and the inject timing into
/// a single per-take breakdown (console + gap-log.csv).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Take {
    text: String,
    /// Length of the recorded audio itself — the denominator every other
    /// number should be read against.
    audio_ms: u64,
    /// capture::stop — mic teardown wait + resample.
    stop_ms: u64,
    engine_wait_ms: u64,
    attempts: u32,
    wav_ms: u64,
    http_ms: u64,
    parse_ms: u64,
    /// Dictionary pass, in MICROseconds — logged to prove it stays free.
    dict_us: u64,
}

#[tauri::command]
async fn stop_and_transcribe(
    state: tauri::State<'_, capture::CaptureState>,
    rules: tauri::State<'_, dictionary::Rules>,
) -> Result<Take, String> {
    let t = std::time::Instant::now();
    let samples = capture::stop(&state)?;
    let stop_ms = t.elapsed().as_millis() as u64;
    let audio_ms = samples.len() as u64 * 1000 / capture::TARGET_SAMPLE_RATE as u64;
    println!(
        "[sayit] captured {:.1}s of audio",
        samples.len() as f32 / capture::TARGET_SAMPLE_RATE as f32
    );
    // Patient: if the take raced an engine wake, wait for it. 90s covers
    // even a first-ever CUDA warmup.
    let (text, timing) =
        transcribe::transcribe_waiting(samples, std::time::Duration::from_secs(90)).await?;
    println!("[sayit] transcribed: {text:?}");
    // The dictionary pass: fix the model's known mishearings before anyone
    // downstream sees the text. In-memory rules, microsecond cost.
    let t = std::time::Instant::now();
    let corrected = dictionary::apply(&rules.0.lock().unwrap(), &text);
    let dict_us = t.elapsed().as_micros() as u64;
    if corrected != text {
        println!("[sayit] dictionary: {corrected:?}");
    }
    println!(
        "[timing] stop_and_transcribe: stop {stop_ms}ms · wait {}ms · wav {}ms · inference {}ms · \
         parse {}ms · dict {dict_us}µs — for {:.1}s of audio",
        timing.engine_wait_ms,
        timing.wav_ms,
        timing.http_ms,
        timing.parse_ms,
        audio_ms as f32 / 1000.0
    );
    Ok(Take {
        text: corrected,
        audio_ms,
        stop_ms,
        engine_wait_ms: timing.engine_wait_ms,
        attempts: timing.attempts,
        wav_ms: timing.wav_ms,
        http_ms: timing.http_ms,
        parse_ms: timing.parse_ms,
        dict_us,
    })
}

#[cfg(windows)]
#[tauri::command]
async fn inject_text(text: String) -> Result<inject::InjectTiming, String> {
    tauri::async_runtime::spawn_blocking(move || inject::inject(&text))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "linux")]
#[tauri::command]
async fn inject_text(
    text: String,
    input: tauri::State<'_, linux_input::LinuxInput>,
) -> Result<inject::InjectTiming, String> {
    let input = input.inner().clone();
    tauri::async_runtime::spawn_blocking(move || inject::inject(&text, &input))
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

#[tauri::command]
fn trigger_mode(mode: hotkey::TriggerMode, control: tauri::State<hotkey::HotkeyControl>) {
    hotkey::set_mode(&control, mode);
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlatformInfo {
    os: &'static str,
    trigger_hint: &'static str,
}

#[tauri::command]
fn platform_info() -> PlatformInfo {
    #[cfg(windows)]
    return PlatformInfo {
        os: "windows",
        trigger_hint: "hold F9 to dictate",
    };
    #[cfg(target_os = "linux")]
    return PlatformInfo {
        os: "linux",
        trigger_hint: "double-tap left Ctrl or remapped Caps to dictate",
    };
}

/// The coordinator pulls this at boot so its sleep timer honors the
/// persisted keep-awake preference.
#[tauri::command]
fn get_keep_awake(app: tauri::AppHandle) -> bool {
    settings::load(&app).keep_awake
}

/// How long the engine may idle before it sleeps (settings.json
/// `idle_minutes`, default 5, 0 = never). Pulled by the coordinator at
/// boot, same as keep-awake.
#[tauri::command]
fn get_idle_minutes(app: tauri::AppHandle) -> u64 {
    settings::load(&app).idle_minutes
}

#[tauri::command]
fn engine_start(app: tauri::AppHandle) -> Result<(), String> {
    sidecar::start(&app)
}

#[tauri::command]
fn engine_sleep(app: tauri::AppHandle) {
    sidecar::sleep(&app);
}

/// One take's full timing breakdown, assembled by the coordinator (it's
/// the only place all three clocks meet: key-up stamp, Rust stage timings,
/// inject timings). All milliseconds except dict_us.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GapRow {
    /// Key-up → inject returned. The number the old log recorded.
    total_ms: u64,
    /// Key-up → text visible on screen. The gap the user actually feels.
    felt_ms: u64,
    chars: usize,
    audio_ms: u64,
    /// Key-up OS event → coordinator handler entry (webview event hop).
    /// i64: two clocks are subtracted, so rounding may make it -1.
    dispatch_ms: i64,
    /// Coordinator housekeeping around the two pipeline commands: the
    /// show() calls' tray + waveform IPC, before stop_and_transcribe and
    /// between it and inject_text.
    pre_ms: u64,
    stop_ms: u64,
    engine_wait_ms: u64,
    wav_ms: u64,
    http_ms: u64,
    parse_ms: u64,
    dict_us: u64,
    /// Invoke round-trip minus the Rust-side accounted stages: the
    /// webview↔Rust IPC toll, both commands combined.
    ipc_ms: u64,
    inject_visible_ms: u64,
    inject_total_ms: u64,
}

const GAP_HEADER: &str = "unix_ts,total_ms,felt_ms,chars,audio_ms,dispatch_ms,pre_ms,stop_ms,\
engine_wait_ms,wav_ms,http_ms,parse_ms,dict_us,ipc_ms,inject_visible_ms,inject_total_ms";

/// The gap, measured, not vibed — now itemized: release-to-text-landed per
/// take, split across every stage, appended to a CSV in the app config dir
/// (next to settings.json — it's this machine's diary) so tuning has a
/// dataset. A pre-breakdown log file (three columns) is shelved aside as
/// gap-log-v1.csv rather than mixed into the new schema.
#[tauri::command]
fn log_gap(app: tauri::AppHandle, row: GapRow) {
    println!(
        "[sayit] gap: felt {}ms (total {}ms) for {} chars",
        row.felt_ms, row.total_ms, row.chars
    );
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let csv = dir.join("gap-log.csv");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Schema check: an existing file whose header isn't ours gets renamed,
    // not appended to — mixed-width rows would poison the dataset.
    if let Ok(existing) = std::fs::File::open(&csv) {
        use std::io::BufRead;
        let first = std::io::BufReader::new(existing)
            .lines()
            .next()
            .and_then(|l| l.ok())
            .unwrap_or_default();
        if first.trim() != GAP_HEADER {
            let mut shelf = dir.join("gap-log-v1.csv");
            if shelf.exists() {
                shelf = dir.join(format!("gap-log-v1-{stamp}.csv"));
            }
            match std::fs::rename(&csv, &shelf) {
                Ok(()) => println!("[sayit] gap-log: old schema shelved as {}", shelf.display()),
                Err(e) => {
                    // Never risk a poisoned dataset: better to drop one row
                    // than append 16 columns under a 3-column header.
                    eprintln!("[sayit] gap-log: can't shelve old log ({e}); skipping this row");
                    return;
                }
            }
        }
    }

    let new = !csv.exists();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(csv)
    {
        if new {
            let _ = writeln!(file, "{GAP_HEADER}");
        }
        let _ = writeln!(
            file,
            "{stamp},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            row.total_ms,
            row.felt_ms,
            row.chars,
            row.audio_ms,
            row.dispatch_ms,
            row.pre_ms,
            row.stop_ms,
            row.engine_wait_ms,
            row.wav_ms,
            row.http_ms,
            row.parse_ms,
            row.dict_us,
            row.ipc_ms,
            row.inject_visible_ms,
            row.inject_total_ms
        );
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

#[cfg(target_os = "linux")]
pub fn update_preflight() -> Result<(), String> {
    update::preflight()
}

#[cfg(not(target_os = "linux"))]
pub fn update_preflight() -> Result<(), String> {
    Ok(())
}

/// One sayit per machine. Two instances means two engines fighting over
/// one hotkey, one port, and — the expensive part — double the VRAM.
/// A named mutex is the classic Windows answer; the handle is deliberately
/// leaked so the claim lasts exactly as long as the process. The
/// single-instance plugin below is the cross-platform half — the mutex
/// wins the race before Tauri even builds.
#[cfg(windows)]
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\sayit-single-instance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        !handle.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

pub fn run() {
    #[cfg(windows)]
    if already_running() {
        eprintln!("[sayit] another sayit is already running — not starting a second engine");
        return;
    }
    let builder = tauri::Builder::default()
        // Must be the first plugin: a second process must never race the
        // sidecar, updater, clipboard, or event-device listener.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if !settings::load(app).setup_complete && cfg!(target_os = "linux") {
                setup::show(app);
            } else {
                tray::set_status(app, "already running");
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(capture::CaptureState::default())
        .manage(sidecar::Sidecar(Mutex::default()))
        .manage(sidecar::Ready::default())
        .manage(sounds::start())
        .manage(tray::Tray::default())
        .manage(MicChoice(Mutex::new(None)))
        .manage(dictionary::Rules::default())
        .manage(hotkey::HotkeyControl::default())
        .manage(setup::SetupState::default());

    #[cfg(windows)]
    let builder = builder.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_shortcuts([hotkey::PUSH_TO_TALK])
            .expect("push-to-talk key is not a valid shortcut")
            .with_handler(|app, _shortcut, event| hotkey::on_shortcut(app, event.state()))
            .build(),
    );
    #[cfg(target_os = "linux")]
    let builder = builder.manage(linux_input::LinuxInput::start());

    builder
        .setup(|app| {
            let saved = settings::load(app.handle());
            *app.state::<MicChoice>().0.lock().unwrap() = saved.microphone.clone();
            dictionary::init(app.handle(), &saved);
            setup::init_window(app.handle());
            tray::build(app.handle(), &saved)
                .map_err(|e| std::io::Error::other(format!("tray failed: {e}")))?;
            // A previous sayit that died uncleanly may have left its engine
            // behind, still holding VRAM and port 8642. Clear it before
            // spawning ours, or we'd run two (docs/sidecar.md).
            #[cfg(windows)]
            sidecar::reap_stale();
            setup::start_normal(app.handle());
            tauri::async_runtime::spawn(update::check_and_install(app.handle().clone()));
            #[cfg(windows)]
            println!("[sayit] push-to-talk on {}", hotkey::PUSH_TO_TALK);
            #[cfg(target_os = "linux")]
            println!("[sayit] trigger: double-tap left Ctrl/remapped Caps; one tap stops");
            update::mark_healthy();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_capture,
            stop_and_transcribe,
            inject_text,
            play_sound,
            tray_status,
            is_ready,
            trigger_mode,
            platform_info,
            get_keep_awake,
            get_idle_minutes,
            engine_start,
            engine_sleep,
            log_gap,
            waveform_show,
            waveform_hide,
            dictionary::dictionary_rules,
            dictionary::dictionary_save,
            dictionary::dictionary_preview,
            dictionary::dictionary_show,
            dictionary::dictionary_hide,
            setup::setup_show,
            setup::setup_hide,
            setup::setup_snapshot,
            setup::setup_begin,
            setup::setup_finish,
            setup::setup_test_injection
        ])
        .build(tauri::generate_context!())
        .expect("error building sayit")
        .run(|app, event| {
            // The sidecar is our child; if we exit and leave it running, it
            // squats on the port and half a gig of VRAM forever. The job
            // object (sidecar.rs) would catch it anyway — this is just the
            // polite version that doesn't wait for the OS.
            if let tauri::RunEvent::Exit = event {
                sidecar::stop(app);
            }
        });
}
