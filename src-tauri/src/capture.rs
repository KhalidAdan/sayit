//! Stage 2: the microphone. Records while told to, and hands back audio in
//! the one format the rest of the pipeline speaks: 16kHz mono f32.
//!
//! cpal's Stream is not Send, so each recording session runs on its own
//! thread that builds the stream, holds it, and drops it there. The audio
//! callback runs on an OS realtime thread — it copies samples out and does
//! nothing else.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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
}

pub fn start(state: &CaptureState) -> Result<(), String> {
    let mut capture = state.lock().unwrap();
    if capture.session.is_some() {
        return Err("already recording".into());
    }

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no microphone found")?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();
    let source_rate = config.sample_rate.0;
    let channels = config.channels as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::default();
    let (stop, stopped) = channel::<()>();

    let sink = samples.clone();
    let thread = std::thread::spawn(move || {
        let err_fn = |err| eprintln!("[capture] stream error: {err}");
        // The mic may deliver f32 or i16 depending on the driver; both paths
        // downmix interleaved frames to mono by averaging the channels.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    let mut sink = sink.lock().unwrap();
                    for frame in data.chunks(channels) {
                        sink.push(frame.iter().sum::<f32>() / channels as f32);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    let mut sink = sink.lock().unwrap();
                    for frame in data.chunks(channels) {
                        let sum: f32 = frame.iter().map(|&s| s as f32 / 32768.0).sum();
                        sink.push(sum / channels as f32);
                    }
                },
                err_fn,
                None,
            ),
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

    let _ = session.stop.send(());
    let _ = session.thread.join(); // stream fully closed: the buffer is final
    let recorded = std::mem::take(&mut *session.samples.lock().unwrap());
    Ok(resample(&recorded, session.source_rate, TARGET_SAMPLE_RATE))
}

/// Linear-interpolation resampler. Crude by DSP standards, transparent by
/// ours: each output sample sits between two input samples, weighted by
/// where it falls. Speech survives 48k→16k this way just fine, and every
/// line is explainable — which is the point.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
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
