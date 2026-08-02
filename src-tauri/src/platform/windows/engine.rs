//! The engine's crash-tie and boot-time hygiene, Win32 edition: a
//! kill-on-close job object plus a toolhelp sweep for orphans left by
//! previous runs.

use std::process::Child;

/// The job object every spawned engine is assigned to. Its one limit is
/// KILL_ON_JOB_CLOSE: when sayit dies — cleanly, by crash, by Task
/// Manager, by a dev run torn down mid-flight — the OS closes our handle,
/// the job closes, and whisper-server dies with us. This is the crash-safe
/// backstop behind the polite kills in sidecar::sleep and RunEvent::Exit;
/// without it an orphaned engine squats ~500MB of VRAM until someone
/// notices (2026-08-02: two orphans starved a nightly Ollama run on this
/// machine). The handle is created once and deliberately never closed — it
/// must live exactly as long as the process.
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

/// Tie the engine's lifetime to ours BEFORE it can outlive a crash.
/// A failed assign is logged, not fatal: the engine still works, it's
/// just back to trusting the exit handler (and the next boot's reap).
pub(super) fn assign_to_job(child: &Child) {
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
pub(super) fn reap_stale() {
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
