// Prevents an additional console window on Windows release builds.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os().any(|arg| arg == "--update-probe") {
        return;
    }
    if let Err(error) = sayit_lib::update_preflight() {
        eprintln!("[update] rollback preflight failed: {error}");
    }

    #[cfg(target_os = "linux")]
    {
        // GNOME Wayland intentionally forbids top-level positioning. sayit's
        // non-focusable waveform has a physical place (bottom centre), so the
        // tiny Tauri UI runs through XWayland while input/audio remain native.
        if std::env::var_os("DISPLAY").is_some()
            && std::env::var_os("SAYIT_NATIVE_WAYLAND").is_none()
        {
            std::env::set_var("GDK_BACKEND", "x11");
            // WebKitGTK's DMA-BUF path is unreliable through NVIDIA XWayland
            // (failed GBM allocations render an empty setup window).
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }
    sayit_lib::run()
}
