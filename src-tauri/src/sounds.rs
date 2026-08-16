//! The key's voice. Plays user-supplied sounds from soundpack/ — press,
//! refuse, accept — as audible feedback for the state machine. Sounds are
//! a hook, not an asset: a missing file is a silent slot, and silence is a
//! legitimate choice. See soundpack/README.md.
//!
//! One thread owns both the output stream and the active sink. A synchronous
//! Stop command lets capture silence completion speech before opening the
//! microphone, so feedback can never become the next transcript.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;
use tauri::Manager;

const SLOTS: [&str; 3] = ["press", "refuse", "accept"];
const EXTENSIONS: [&str; 3] = ["wav", "ogg", "mp3"]; // first match wins

fn should_play(slot: &str, completion_voice: bool) -> bool {
    slot != "accept" || completion_voice
}

enum Command {
    Play(String),
    Stop(Sender<()>),
}

pub struct Sounds {
    tx: Sender<Command>,
    completion_voice: AtomicBool,
    accept_available: bool,
}

impl Sounds {
    pub fn play(&self, slot: &str) -> bool {
        if !should_play(slot, self.completion_voice.load(Ordering::Relaxed)) {
            return false;
        }
        self.tx.send(Command::Play(slot.to_string())).is_ok()
    }

    /// Stop playback before capture starts. Normal acknowledgement is nearly
    /// immediate; sound failure must never wedge the microphone indefinitely.
    pub fn stop(&self) {
        let (ack_tx, ack_rx) = channel();
        if self.tx.send(Command::Stop(ack_tx)).is_ok()
            && ack_rx.recv_timeout(Duration::from_millis(250)).is_err()
        {
            eprintln!("[sayit] sound stop timed out; continuing without blocking capture");
        }
    }

    pub fn accept_available(&self) -> bool {
        self.accept_available
    }

    pub fn completion_voice(&self) -> bool {
        self.completion_voice.load(Ordering::Relaxed)
    }

    /// Returns the effective state. A missing accept clip cannot be enabled.
    pub fn set_completion_voice(&self, enabled: bool) -> bool {
        let effective = enabled && self.accept_available;
        self.completion_voice.store(effective, Ordering::Relaxed);
        effective
    }
}

/// One shared mutation path for both native tray implementations: update the
/// live gate first, then persist the preference without making sound depend on
/// a successful disk write.
pub fn set_completion_voice(app: &tauri::AppHandle, enabled: bool) -> bool {
    let effective = app.state::<Sounds>().set_completion_voice(enabled);
    let mut saved = crate::settings::load(app);
    saved.completion_voice = effective;
    crate::settings::save(app, &saved);
    println!("[sayit] completion voice: {effective}");
    effective
}

fn load() -> HashMap<&'static str, Vec<u8>> {
    let mut loaded = HashMap::new();
    if let Some(dir) = crate::paths::soundpack_dir() {
        for slot in SLOTS {
            for ext in EXTENSIONS {
                if let Ok(bytes) = std::fs::read(dir.join(format!("{slot}.{ext}"))) {
                    loaded.insert(slot, bytes);
                    break;
                }
            }
        }
    }
    loaded
}

fn log_loaded(loaded: &HashMap<&'static str, Vec<u8>>) {
    println!(
        "[sayit] soundpack: {}",
        SLOTS
            .map(|slot| format!(
                "{slot}={}",
                if loaded.contains_key(slot) {
                    "loaded"
                } else {
                    "silent"
                }
            ))
            .join(", ")
    );
}

fn silent_loop(rx: Receiver<Command>) {
    while let Ok(command) = rx.recv() {
        if let Command::Stop(ack) = command {
            let _ = ack.send(());
        }
    }
}

/// Loads every present slot into memory (they're tiny; disk reads at press
/// time would cost gap) and parks a thread waiting for play orders.
pub fn start() -> Sounds {
    let loaded = load();
    let accept_available = loaded.contains_key("accept");
    log_loaded(&loaded);

    let (tx, rx) = channel::<Command>();
    std::thread::spawn(move || {
        // If there's no audio output device, sounds degrade to silence —
        // capture still receives Stop acknowledgements and never depends on
        // the speaker existing.
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            eprintln!("[sayit] no audio output; all slots silent");
            silent_loop(rx);
            return;
        };

        let mut active: Option<rodio::Sink> = None;
        while let Ok(command) = rx.recv() {
            match command {
                Command::Play(slot) => {
                    let Some(bytes) = loaded.get(slot.as_str()) else {
                        continue;
                    };
                    match rodio::Decoder::new(Cursor::new(bytes.clone())) {
                        Ok(source) => match rodio::Sink::try_new(&handle) {
                            Ok(sink) => {
                                if let Some(previous) = active.take() {
                                    previous.stop();
                                }
                                sink.append(source);
                                active = Some(sink);
                            }
                            Err(e) => eprintln!("[sayit] sound '{slot}' failed to play: {e}"),
                        },
                        Err(e) => eprintln!("[sayit] sound '{slot}' failed to decode: {e}"),
                    }
                }
                Command::Stop(ack) => {
                    if let Some(sink) = active.take() {
                        sink.stop();
                    }
                    let _ = ack.send(());
                }
            }
        }
    });

    Sounds {
        tx,
        completion_voice: AtomicBool::new(false),
        accept_available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_voice_gates_only_accept() {
        assert!(!should_play("accept", false));
        assert!(should_play("accept", true));
        assert!(should_play("press", false));
        assert!(should_play("refuse", false));
    }
}
