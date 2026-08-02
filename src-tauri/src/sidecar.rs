//! The engine's keeper: whisper-server's full lifecycle. Started at boot,
//! put to sleep when the coordinator says so (idle timer), woken on
//! demand, killed at exit — and, via a Windows job object, killed by the
//! OS itself if sayit dies any less politely. Sleeping frees ~500MB of
//! VRAM; waking costs seconds (the GPU driver caches compiled kernels
//! after the first-ever run). The user never manages any of this — the key
//! always works, and the tray narrates.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

use crate::{capture, transcribe};

/// The job object every spawned engine is assigned to. Its one limit is
/// KILL_ON_JOB_CLOSE: when sayit dies — cleanly, by crash, by Task
/// Manager, by a dev run torn down mid-flight — the OS closes our handle,
/// the job closes, and whisper-server dies with us. This is the crash-safe
/// backstop behind the polite kills in sleep() and RunEvent::Exit; without
/// it an orphaned engine squats ~500MB of VRAM until someone notices
/// (2026-08-02: two orphans starved a nightly Ollama run on this machine).
/// The handle is created once and deliberately never closed — it must live
/// exactly as long as the process.
#[cfg(windows)]
fn job() -> windows_sys::Win32::Foundation::HANDLE {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    static JOB: OnceLock<isize> = OnceLock::new();
    *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            eprintln!("[sayit] couldn't create job object — engine won't be crash-tied to us");
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            eprintln!("[sayit] couldn't set kill-on-close on the job object");
        }
        job as isize
    }) as windows_sys::Win32::Foundation::HANDLE
}

/// Kill any whisper-server.exe left over from a previous sayit that died
/// without cleaning up (pre-job-object builds, or a failed job assign).
/// Called once at boot, before the first spawn. Without this, the orphan
/// keeps its VRAM AND its port — whisper-server binds with SO_REUSEADDR,
/// which on Windows lets our fresh instance bind port 8642 *alongside* the
/// stale one instead of failing. That's how "two whisper-server.exe at
/// once" happens: nothing crashes, both hold ~500MB, and requests land on
/// whichever the OS feels like.
///
/// Only processes whose image path is exactly OUR resolved sidecar exe are
/// touched — a whisper-server belonging to some other tool is not ours to
/// kill.
#[cfg(windows)]
pub fn reap_stale() {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    // Compare canonicalized, lowercased paths: the snapshot reports plain
    // "C:\..." while our resolver may hold a relative or \\?\ form.
    let Some(ours) = crate::paths::sidecar_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .map(|p| p.to_string_lossy().to_lowercase())
    else {
        return;
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
            if name.eq_ignore_ascii_case("whisper-server.exe") {
                let proc = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    entry.th32ProcessID,
                );
                if !proc.is_null() {
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    if QueryFullProcessImageNameW(proc, 0, buf.as_mut_ptr(), &mut len) != 0 {
                        let path = String::from_utf16_lossy(&buf[..len as usize]);
                        let theirs = std::fs::canonicalize(&path)
                            .map(|p| p.to_string_lossy().to_lowercase())
                            .unwrap_or_else(|_| path.to_lowercase());
                        if theirs == ours {
                            println!(
                                "[sayit] reaping stale whisper-server (pid {}) from a previous run",
                                entry.th32ProcessID
                            );
                            TerminateProcess(proc, 1);
                        }
                    }
                    CloseHandle(proc);
                }
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
}

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
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(target_os = "linux")]
    {
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
    let child = command
        .spawn()
        .map_err(|e| format!("failed to spawn whisper-server: {e}"))?;
    // Tie the engine's lifetime to ours BEFORE it can outlive a crash.
    // A failed assign is logged, not fatal: the engine still works, it's
    // just back to trusting the exit handler (and the next boot's reap).
    // On Linux the equivalent tie is prctl(PR_SET_PDEATHSIG) above.
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        if windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
            job(),
            child.as_raw_handle() as _,
        ) == 0
        {
            eprintln!("[sayit] couldn't assign whisper-server to the job object");
        }
    }
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
