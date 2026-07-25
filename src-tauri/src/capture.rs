//! Stage 2: the microphone. Records while told to, and hands back audio in
//! the one format the rest of the pipeline speaks: 16kHz mono f32.
//!
//! cpal's Stream is not Send, so each recording session runs on its own
//! thread that builds the stream, holds it, and drops it there. The audio
//! callback runs on an OS realtime thread — it copies samples out, tracks
//! the peak level, and does nothing else. A separate monitor thread emits
//! that level to the webview every 50ms for the waveform.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tauri::{AppHandle, Emitter};

/// What Whisper expects. Everything downstream assumes this rate.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Default)]
pub struct Capture {
    session: Option<Session>,
}

pub type CaptureState = Mutex<Capture>;

struct Session {
    stop: Sender<()>,
    thread: JoinHandle<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    source_rate: u32,
    alive: Arc<AtomicBool>,
}

/// Input device names, for the tray's microphone picker.
pub fn list_inputs() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

pub fn start(state: &CaptureState, app: &AppHandle, preferred: Option<String>) -> Result<(), String> {
    let mut capture = state.lock().unwrap();
    if capture.session.is_some() {
        return Err("already recording".into());
    }

    let host = cpal::default_host();
    // A vanished preferred device (unplugged USB mic) falls back to default
    // rather than refusing to record — losing a take is worse than using
    // the wrong mic.
    let device = preferred
        .as_ref()
        .and_then(|name| {
            host.input_devices()
                .ok()?
                .find(|d| d.name().map(|n| &n == name).unwrap_or(false))
        })
        .or_else(|| host.default_input_device())
        .ok_or("no microphone found")?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();
    let source_rate = config.sample_rate.0;
    let channels = config.channels as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::default();
    let (stop, stopped) = channel::<()>();
    // Peak level of the most recent chunks, stored as f32 bits so the
    // realtime callback never takes a lock.
    let level = Arc::new(AtomicU32::new(0));
    let alive = Arc::new(AtomicBool::new(true));

    // The waveform's data feed: every 50ms, read-and-reset the peak and
    // emit it. Dies with the session.
    {
        let (app, level, alive) = (app.clone(), level.clone(), alive.clone());
        std::thread::spawn(move || {
            while alive.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let peak = f32::from_bits(level.swap(0, Ordering::Relaxed));
                let _ = app.emit("mic_level", peak);
            }
        });
    }

    let sink = samples.clone();
    let thread = std::thread::spawn(move || {
        let err_fn = |err| eprintln!("[capture] stream error: {err}");
        let track = move |cur: &AtomicU32, peak: f32| {
            if peak > f32::from_bits(cur.load(Ordering::Relaxed)) {
                cur.store(peak.to_bits(), Ordering::Relaxed);
            }
        };
        // The mic may deliver f32 or i16 depending on the driver; both paths
        // downmix interleaved frames to mono by averaging the channels.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let level = level.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &_| {
                        let mut sink = sink.lock().unwrap();
                        let mut peak = 0f32;
                        for frame in data.chunks(channels) {
                            let s = frame.iter().sum::<f32>() / channels as f32;
                            peak = peak.max(s.abs());
                            sink.push(s);
                        }
                        track(&level, peak);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let level = level.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &_| {
                        let mut sink = sink.lock().unwrap();
                        let mut peak = 0f32;
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame.iter().map(|&s| s as f32 / 32768.0).sum();
                            let s = sum / channels as f32;
                            peak = peak.max(s.abs());
                            sink.push(s);
                        }
                        track(&level, peak);
                    },
                    err_fn,
                    None,
                )
            }
            other => {
                eprintln!("[capture] unsupported sample format: {other}");
                return;
            }
        };
        match stream {
            Ok(stream) => {
                if stream.play().is_ok() {
                    // Block until stop() signals (or the sender is dropped).
                    let _ = stopped.recv();
                }
                // The stream drops here, on the thread that built it.
            }
            Err(e) => eprintln!("[capture] failed to open stream: {e}"),
        }
    });

    capture.session = Some(Session {
        stop,
        thread,
        samples,
        source_rate,
        alive,
    });
    Ok(())
}

/// Stops recording and returns the whole take as 16kHz mono samples.
pub fn stop(state: &CaptureState) -> Result<Vec<f32>, String> {
    let session = state
        .lock()
        .unwrap()
        .session
        .take()
        .ok_or("not recording")?;

    session.alive.store(false, Ordering::Relaxed);
    let _ = session.stop.send(());
    let _ = session.thread.join(); // stream fully closed: the buffer is final
    let recorded = std::mem::take(&mut *session.samples.lock().unwrap());
    Ok(resample(&recorded, session.source_rate, TARGET_SAMPLE_RATE))
}

/// Linear-interpolation resampler. Crude by DSP standards, transparent by
/// ours: each output sample sits between two input samples, weighted by
/// where it falls. Speech survives 48k→16k this way just fine, and every
/// line is explainable — which is the point.
pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let left = pos as usize;
            let right = (left + 1).min(input.len() - 1);
            let frac = (pos - left as f64) as f32;
            input[left] * (1.0 - frac) + input[right] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_passthrough() {
        let input = vec![0.1, -0.5, 0.9];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn three_to_one_keeps_every_third_sample() {
        // 48k→16k is exactly 3:1: output i sits exactly on input 3i,
        // so interpolation should return those samples untouched.
        let input: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let out = resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 4);
        assert_eq!(out, vec![0.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn output_length_scales_with_ratio() {
        // One second of 44.1kHz audio must come out as ~one second of 16kHz.
        let input = vec![0.0; 44_100];
        let out = resample(&input, 44_100, 16_000);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "got {}", out.len());
    }

    #[test]
    fn interpolates_between_samples() {
        // 2:1 upsample of a ramp: odd outputs land halfway between inputs.
        let out = resample(&[0.0, 1.0], 8_000, 16_000);
        assert_eq!(out.len(), 4);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }
}
