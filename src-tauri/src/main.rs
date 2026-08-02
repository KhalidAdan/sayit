// Prevents an additional console window on Windows release builds.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use sayit_lib::platform::{Current, Platform};

fn main() {
    // The Linux A/B updater probes a staged binary with this flag; it must
    // exit before touching anything.
    if std::env::args_os().any(|arg| arg == "--update-probe") {
        return;
    }
    if let Err(error) = Current::update_preflight() {
        eprintln!("[update] rollback preflight failed: {error}");
    }
    Current::pre_launch();
    sayit_lib::run()
}
