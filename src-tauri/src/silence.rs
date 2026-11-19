//! The silence gate: layer 1 of "silence never pastes words nobody said."
//!
//! Whisper learned to transcribe from subtitled video, and a video's last
//! seconds are usually quiet under a closing line. So on a take with no
//! voice in it, the model doesn't answer "nothing" — it invents the line:
//! "Thank you." "Thanks for watching!" This module refuses such a take
//! before the engine ever sees it. stop_and_transcribe returns empty text,
//! and the coordinator's existing "heard nothing" path takes over. No GPU
//! work, and no waiting for a sleeping engine to wake.
//!
//! Three layers, cheapest first:
//! 1. this energy gate (pure arithmetic on the samples: ~50µs per second
//!    of audio in a release build, so a 5 s take costs about 0.25ms);
//! 2. opt-in (`"vad": true` in settings.json, off by default because it
//!    widens the gap): whisper-server's own voice detector, Silero VAD
//!    (sidecar.rs), for takes loud enough to pass here — a fan, a click;
//! 3. an end-of-text credit strip for what still slips through (credits.rs).
//!
//! The rule is hex's (docs/hex-teardown.md §2.4). The take is cut into
//! 30 ms windows and each window's loudness measured. The take is silence
//! when BOTH hold:
//!
//! - **nothing in it is loud:** its loudest window is under −50 dBFS; and
//! - **nothing in it is sustained:** it has no run of ~180 ms where the
//!   level sits at least 12 dB above the take's own noise floor.
//!
//! If either fails, the take might hold a voice, and it goes to the engine.
//! The gate is built to refuse only what is almost certainly nothing; it
//! never decides "speech."
//!
//! Why two tests? A fixed −50 dBFS line alone would refuse real speech
//! from a quiet built-in mic (hex measured those at −51 to −60 dBFS). The
//! relative test rescues it: quiet speech still stands well clear of its
//! own room's hiss. And "sustained" is what keeps a key click — loud for a
//! few milliseconds, then gone — from passing as a word.
//!
//! Vocabulary, for the reader new to audio:
//! - **dBFS** — decibels relative to full scale. 0 dBFS is the loudest a
//!   sample can be (±1.0); every −20 dB is a tenth of the amplitude. So
//!   −50 dBFS is an RMS of about 0.003 — a whisper across the room.
//! - **RMS** — root mean square: square every sample, average, take the
//!   square root. A window's "average loudness," immune to the sign flips
//!   of a waveform.
//! - **noise floor** — how loud the room is when nobody talks. We take it
//!   from the take itself: the level of its quietest tenth of windows.
//!
//! Known blind spots, both inherited with hex's rule:
//! - a take that is *wall-to-wall* quiet speech with no pause at all has no
//!   quiet tenth, so its floor is the voice itself. Real takes have pauses
//!   (the beat before you speak, the gap between phrases) to measure on;
//! - a single very quiet word can fall under both tests. Measured: a lone
//!   "yes" whose loudest window was -56.6 dBFS (40 dB below a normal take)
//!   cleared its floor for only 120 ms, and was refused here.

use crate::capture::TARGET_SAMPLE_RATE;
use std::time::Instant;

/// One analysis window. Long enough to average several cycles of even a low
/// voice (~100 Hz = 10 ms per cycle), short enough that a word spans many.
const WINDOW_MS: usize = 30;
/// 480 samples at 16 kHz. The gate runs on capture's output, which is
/// always 16 kHz.
const WINDOW: usize = TARGET_SAMPLE_RATE as usize * WINDOW_MS / 1000;

/// Test 1: a window this loud anywhere means "maybe a voice" outright.
const LOUD_DBFS: f32 = -50.0;
/// Test 2: how far above the take's own noise floor a window must rise to
/// count as "something happening there."
const OVER_FLOOR_DB: f32 = 12.0;
/// ...but never quieter than this, however quiet the floor. A mic turned
/// all the way down still dithers; its specks mustn't pass as speech.
const QUIETEST_VOICE_DBFS: f32 = -66.0;
/// Test 2: the run must last 6 windows × 30 ms = 180 ms. hex rounds
/// Silero's 160 ms shortest-speech rule up to whole windows.
const SUSTAINED_WINDOWS: usize = 6;
/// What a window of digital silence (or a constant offset) reports. The
/// log of zero is minus infinity; a floor keeps the arithmetic finite.
const SILENT_DBFS: f32 = -120.0;

/// What the gate measured on one take. Logged every time, so a wrongly
/// refused take can be diagnosed from these numbers alone — no audio is
/// ever kept.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Levels {
    /// The loudest window's level.
    loudest_dbfs: f32,
    /// The quietest tenth's level. None when every window was digital
    /// silence: there is no room tone to measure a floor from.
    noise_floor_dbfs: Option<f32>,
    /// The longest run of windows at least OVER_FLOOR_DB above the floor.
    longest_run_windows: usize,
}

impl Levels {
    fn measure(samples: &[f32]) -> Self {
        let windows: Vec<f32> = samples.chunks(WINDOW).map(window_dbfs).collect();
        let loudest_dbfs = windows.iter().copied().fold(SILENT_DBFS, f32::max);
        let noise_floor_dbfs = noise_floor(&windows);
        let longest_run_windows = noise_floor_dbfs.map_or(0, |floor| {
            let threshold = (floor + OVER_FLOOR_DB).max(QUIETEST_VOICE_DBFS);
            longest_run(&windows, threshold)
        });
        Self {
            loudest_dbfs,
            noise_floor_dbfs,
            longest_run_windows,
        }
    }

    /// Both tests hold: nothing loud, nothing sustained.
    fn is_silence(&self) -> bool {
        self.loudest_dbfs < LOUD_DBFS && self.longest_run_windows < SUSTAINED_WINDOWS
    }
}

/// The gate, as stop_and_transcribe calls it: true means "no voice here —
/// return empty text, don't wake the engine." Logs only levels and timing,
/// never anything about what was said.
pub fn is_silent(samples: &[f32]) -> bool {
    let t = Instant::now();
    let levels = Levels::measure(samples);
    let silent = levels.is_silence();
    let floor = levels
        .noise_floor_dbfs
        .map_or("none (digital silence)".to_string(), |f| {
            format!("{f:.1} dBFS")
        });
    println!(
        "[silence] gate: loudest {:.1} dBFS · floor {floor} · longest run {}ms → {} ({}µs)",
        levels.loudest_dbfs,
        levels.longest_run_windows * WINDOW_MS,
        if silent {
            "silence; engine skipped"
        } else {
            "maybe a voice; to the engine"
        },
        t.elapsed().as_micros()
    );
    silent
}

/// One window's loudness: the RMS of its samples, in dBFS. The window's
/// average (its DC offset) is subtracted first — some mics add a constant
/// to every sample, and a constant isn't sound. Sums run in f64 so a tiny
/// signal riding on a large offset doesn't drown in rounding.
fn window_dbfs(window: &[f32]) -> f32 {
    let n = window.len() as f64;
    let mean = window.iter().map(|&s| s as f64).sum::<f64>() / n;
    let power = window
        .iter()
        .map(|&s| (s as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    // RMS 1e-6 is exactly -120 dBFS, so the clamp and SILENT_DBFS agree.
    (20.0 * power.sqrt().max(1e-6).log10()) as f32
}

/// The take's own noise floor: the 10th-percentile window level. Windows of
/// digital silence are left out — a mic warming up delivers exact zeros,
/// which say nothing about the room.
fn noise_floor(windows: &[f32]) -> Option<f32> {
    let mut audible: Vec<f32> = windows
        .iter()
        .copied()
        .filter(|&level| level > SILENT_DBFS)
        .collect();
    if audible.is_empty() {
        return None;
    }
    audible.sort_by(f32::total_cmp);
    Some(audible[(audible.len() - 1) / 10])
}

/// The most windows in one unbroken run at or above `threshold`. One quiet
/// window inside a run is forgiven (but not counted) — the closure inside
/// a "t" or "k" is a silent instant in the middle of a word. Two quiet
/// windows in a row end the run.
fn longest_run(windows: &[f32], threshold: f32) -> usize {
    let (mut run, mut longest, mut forgiven) = (0, 0, false);
    for &level in windows {
        if level >= threshold {
            run += 1;
            longest = longest.max(run);
            forgiven = false;
        } else if run > 0 && !forgiven {
            forgiven = true;
        } else {
            run = 0;
            forgiven = false;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = TARGET_SAMPLE_RATE as usize;

    fn samples_in(ms: usize) -> usize {
        RATE * ms / 1000
    }

    /// dBFS back to a linear amplitude: 10^(dB/20).
    fn amp(dbfs: f32) -> f32 {
        10f32.powf(dbfs / 20.0)
    }

    /// Room tone: white noise at an RMS level. A tiny xorshift generator
    /// keeps every run identical without pulling in a random crate.
    fn room(len_ms: usize, rms_dbfs: f32) -> Vec<f32> {
        let mut state: u32 = 0x2545_F491;
        // Uniform noise in [-p, p] has an RMS of p/√3.
        let peak = amp(rms_dbfs) * 3f32.sqrt();
        (0..samples_in(len_ms))
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * peak
            })
            .collect()
    }

    /// Add a voice-like burst: a 150 Hz fundamental plus two harmonics (the
    /// rough shape of a vowel), at an RMS level, from `at_ms` for `len_ms`.
    fn add_voice(take: &mut [f32], at_ms: usize, len_ms: usize, rms_dbfs: f32) {
        // Harmonics at 1, 1/2, 1/4 amplitude: RMS = √((1 + ¼ + 1/16) / 2).
        let scale = amp(rms_dbfs) / (1.3125f32 / 2.0).sqrt();
        let start = samples_in(at_ms);
        for i in 0..samples_in(len_ms) {
            let t = i as f32 / RATE as f32;
            let phase = 2.0 * std::f32::consts::PI * 150.0 * t;
            take[start + i] +=
                scale * (phase.sin() + 0.5 * (2.0 * phase).sin() + 0.25 * (3.0 * phase).sin());
        }
    }

    /// Add a key click: 2 ms of a sharply decaying spike, `peak_dbfs` at
    /// its loudest sample.
    fn add_click(take: &mut [f32], at_ms: usize, peak_dbfs: f32) {
        let start = samples_in(at_ms);
        for i in 0..32 {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            take[start + i] += sign * amp(peak_dbfs) * (-(i as f32) / 8.0).exp();
        }
    }

    #[test]
    fn digital_zeros_are_silence() {
        let levels = Levels::measure(&vec![0.0; samples_in(2000)]);
        assert!(levels.is_silence());
        assert_eq!(levels.noise_floor_dbfs, None, "zeros have no room tone");
        assert_eq!(levels.loudest_dbfs, SILENT_DBFS);
    }

    #[test]
    fn a_constant_offset_is_not_sound() {
        // A mic stuck at a DC offset delivers a constant, not a waveform.
        let levels = Levels::measure(&vec![0.02; samples_in(1000)]);
        assert!(levels.is_silence());
        assert_eq!(levels.noise_floor_dbfs, None);
    }

    #[test]
    fn low_room_noise_is_silence() {
        let levels = Levels::measure(&room(3000, -65.0));
        assert!(levels.is_silence(), "{levels:?}");
        let floor = levels.noise_floor_dbfs.unwrap();
        assert!((floor - -65.0).abs() < 1.5, "floor measured {floor}");
    }

    #[test]
    fn a_loud_click_in_room_noise_is_still_silence() {
        // Its peak sample is -34 dBFS, 36 dB over the room — "loud" by ear.
        // But it lasts 2 ms: averaged over its 30 ms window it stays under
        // the -50 line, and one window is far short of a 180 ms run.
        let mut take = room(2000, -70.0);
        add_click(&mut take, 1005, -34.0);
        let levels = Levels::measure(&take);
        assert!(levels.is_silence(), "{levels:?}");
        assert!(levels.longest_run_windows >= 1, "the click did register");
    }

    #[test]
    fn typing_near_the_mic_is_still_silence() {
        // Ten clicks, 150 ms apart: each is a lone loud window with quiet
        // windows between, so no run ever forms.
        let mut take = room(2500, -70.0);
        for k in 0..10 {
            add_click(&mut take, 505 + 150 * k, -34.0);
        }
        assert!(Levels::measure(&take).is_silence());
    }

    #[test]
    fn a_short_blip_is_too_short_to_be_a_word() {
        // A 100 ms murmur well over the floor: a lip smack, a chair creak.
        let mut take = room(2000, -75.0);
        add_voice(&mut take, 1000, 100, -56.0);
        let levels = Levels::measure(&take);
        assert!(levels.is_silence(), "{levels:?}");
    }

    #[test]
    fn a_quiet_voice_passes_on_its_own_noise_floor() {
        // The built-in-mic case: never reaches -50 dBFS, but stands 19 dB
        // over its room for 400 ms. Test 1 holds; test 2 rescues it.
        let mut take = room(2000, -75.0);
        add_voice(&mut take, 800, 400, -56.0);
        let levels = Levels::measure(&take);
        assert!(levels.loudest_dbfs < LOUD_DBFS, "{levels:?}");
        assert!(!levels.is_silence(), "{levels:?}");
    }

    #[test]
    fn a_300ms_word_passes_loud_or_quiet() {
        // A normal voice: over -50 dBFS, so test 1 alone lets it through.
        let mut loud = room(1500, -65.0);
        add_voice(&mut loud, 600, 300, -30.0);
        assert!(!Levels::measure(&loud).is_silence());

        // The same "yes" from a quiet mic: through on the sustained run.
        let mut quiet = room(1500, -75.0);
        add_voice(&mut quiet, 600, 300, -56.0);
        let levels = Levels::measure(&quiet);
        assert!(levels.loudest_dbfs < LOUD_DBFS);
        assert!(!levels.is_silence(), "{levels:?}");
    }

    #[test]
    fn loud_non_speech_is_the_vads_call_not_ours() {
        // The gate's honest boundary: it only refuses what is certainly
        // nothing. A click loud enough to cross -50 dBFS in its window, or a
        // steady fan, goes to the engine — where Silero VAD (sidecar.rs),
        // when opted in, tells noise from speech.
        let mut click = room(2000, -70.0);
        add_click(&mut click, 1005, -10.0);
        assert!(!Levels::measure(&click).is_silence());
        assert!(!Levels::measure(&room(2000, -45.0)).is_silence());
    }

    #[test]
    fn one_quiet_window_inside_a_run_is_forgiven_not_counted() {
        let (hi, lo) = (-40.0, -80.0);
        // A "k" closure mid-word: the run survives the dip; the dip itself
        // doesn't count toward its length.
        assert_eq!(longest_run(&[hi, hi, lo, hi, hi], -50.0), 4);
        // Two quiet windows in a row end it.
        assert_eq!(longest_run(&[hi, hi, lo, lo, hi, hi], -50.0), 2);
        // A quiet window before any run doesn't use up the forgiveness.
        assert_eq!(longest_run(&[lo, hi, lo, hi], -50.0), 2);
        assert_eq!(longest_run(&[], -50.0), 0);
    }

    #[test]
    fn noise_floor_is_the_quietest_tenth_ignoring_zeros() {
        let mut windows = vec![SILENT_DBFS; 5]; // a mic warming up
        windows.extend((0..10).map(|i| -70.0 + i as f32)); // -70 ..= -61
        assert_eq!(noise_floor(&windows), Some(-70.0));
        assert_eq!(noise_floor(&[SILENT_DBFS; 3]), None);
    }

    #[test]
    fn window_level_matches_the_definition() {
        // A full-scale square wave has RMS 1.0: exactly 0 dBFS.
        let square: Vec<f32> = (0..WINDOW)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        assert!(window_dbfs(&square).abs() < 1e-3);
        // A tenth of that amplitude is -20 dBFS.
        let tenth: Vec<f32> = square.iter().map(|s| s * 0.1).collect();
        assert!((window_dbfs(&tenth) - -20.0).abs() < 1e-3);
    }
}
