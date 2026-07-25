//! The key's voice. Plays user-supplied sounds from soundpack/ — press,
//! refuse, accept — as audible feedback for the state machine. Sounds are
//! a hook, not an asset: a missing file is a silent slot, and silence is a
//! legitimate choice. See soundpack/README.md.
//!
//! Same shape as capture.rs, for the same reason: rodio's output stream
//! can't leave the thread that created it, so one thread owns the speaker
//! for the app's lifetime and everyone else sends it slot names.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::mpsc::{channel, Sender};

const SLOTS: [&str; 3] = ["press", "refuse", "accept"];
const EXTENSIONS: [&str; 3] = ["wav", "ogg", "mp3"]; // first match wins

pub struct Sounds(Sender<String>);

impl Sounds {
    pub fn play(&self, slot: &str) {
        let _ = self.0.send(slot.to_string());
    }
}

/// Loads every present slot into memory (they're tiny; disk reads at press
/// time would cost gap) and parks a thread waiting for play orders.
pub fn start() -> Sounds {
    let (tx, rx) = channel::<String>();

    std::thread::spawn(move || {
        let mut loaded: HashMap<&'static str, Vec<u8>> = HashMap::new();
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
        println!(
            "[sayit] soundpack: {}",
            SLOTS
                .map(|s| format!("{s}={}", if loaded.contains_key(s) { "loaded" } else { "silent" }))
                .join(", ")
        );

        // If there's no audio output device, sounds degrade to silence —
        // the pipeline itself must never depend on the speaker existing.
        let Ok((_stream, handle)) = rodio::OutputStream::try_default() else {
            eprintln!("[sayit] no audio output; all slots silent");
            while rx.recv().is_ok() {}
            return;
        };

        while let Ok(slot) = rx.recv() {
            if let Some(bytes) = loaded.get(slot.as_str()) {
                match rodio::Decoder::new(Cursor::new(bytes.clone())) {
                    Ok(source) => {
                        use rodio::Source;
                        let _ = handle.play_raw(source.convert_samples());
                    }
                    Err(e) => eprintln!("[sayit] sound '{slot}' failed to decode: {e}"),
                }
            }
        }
    });

    Sounds(tx)
}
