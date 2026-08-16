//! Stage 2: the microphone. Records while told to, and hands back audio in
//! the one format the rest of the pipeline speaks: 16kHz mono f32.
//!
//! cpal's Stream is not Send, so each recording session runs on its own
//! thread that builds the stream, holds it, and drops it there. The audio
//! callback runs on an OS realtime thread — it copies samples out, tracks
//! the peak level, and does nothing else. A separate monitor thread emits
//! that level to the webview every 50ms for the waveform.
//!
//! The stop doesn't always cut at key-up: if you were still talking as
//! the key came up, it listens until the word ends (capture/tail.rs).

mod tail;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tauri::{AppHandle, Emitter};

/// What Whisper expects. Everything downstream assumes this rate.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// How often the release tail checks for new audio while it listens. The
/// driver hands audio over in ~10ms blocks, so polling faster finds
/// nothing new.
const TAIL_POLL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Default)]
pub struct Capture {
    session: Option<Session>,
}

pub type CaptureState = Mutex<Capture>;

struct Session {
    stop: Sender<()>,
    ack: Receiver<()>,
    thread: JoinHandle<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    source_rate: u32,
    alive: Arc<AtomicBool>,
}

/// A failed native start owns every worker it already launched. In particular,
/// the waveform monitor must not survive and keep emitting empty bars.
fn complete_start(
    ready: Result<(), String>,
    alive: &AtomicBool,
    stop: &Sender<()>,
) -> Result<(), String> {
    if let Err(error) = ready {
        alive.store(false, Ordering::Relaxed);
        let _ = stop.send(());
        return Err(error);
    }
    Ok(())
}

/// Input device names, for the tray's microphone picker.
pub fn list_inputs() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

pub fn start(
    state: &CaptureState,
    app: &AppHandle,
    preferred: Option<String>,
) -> Result<(), String> {
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
    let device_name = device.name().unwrap_or_else(|_| "microphone".into());
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();
    let source_rate = config.sample_rate.0;
    let channels = config.channels as usize;
    println!("[capture] opening {device_name:?}: {channels}ch @ {source_rate}Hz {sample_format}");

    let samples: Arc<Mutex<Vec<f32>>> = Arc::default();
    let (stop, stopped) = channel::<()>();
    // Bounded-trust handshakes: the thread reports "stream is live" and
    // "stream is torn down", and we WAIT WITH TIMEOUTS. A replugged USB
    // mic can hang WASAPI mid-build or mid-teardown; an unbounded wait
    // there once wedged the whole coordinator (see regression ledger).
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();
    let (ack_tx, ack_rx) = channel::<()>();
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
                // A failed startup or completed take may have stopped the
                // session while this monitor slept. Notice that before the
                // next ordinary emission.
                if !alive.load(Ordering::Relaxed) {
                    break;
                }
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
                let callback_ready = ready_tx.clone();
                let announced = Arc::new(AtomicBool::new(false));
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &_| {
                        if !data.is_empty() && !announced.swap(true, Ordering::Relaxed) {
                            let _ = callback_ready.send(Ok(()));
                        }
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
                let callback_ready = ready_tx.clone();
                let announced = Arc::new(AtomicBool::new(false));
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &_| {
                        if !data.is_empty() && !announced.swap(true, Ordering::Relaxed) {
                            let _ = callback_ready.send(Ok(()));
                        }
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
                let _ = ready_tx.send(Err(format!("unsupported sample format: {other}")));
                return;
            }
        };
        match stream {
            Ok(stream) => match stream.play() {
                Ok(()) => {
                    // The callback, not play(), declares readiness. ALSA can
                    // accept play() while a wedged USB/PipeWire source delivers
                    // no frames at all.
                    let _ = stopped.recv();
                    drop(stream); // torn down on the thread that built it
                    let _ = ack_tx.send(());
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("mic stream won't start: {e}")));
                }
            },
            Err(e) => {
                let _ = ready_tx.send(Err(format!("failed to open mic stream: {e}")));
            }
        }
    });

    // If the stream isn't live within 3s, the take is refused loudly now
    // rather than recording silence into a hung device.
    let ready = match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(result) => result,
        Err(_) => Err("microphone did not start (device hung after replug?)".into()),
    };
    // The level monitor starts before the native stream so it cannot miss the
    // first callback. Failed starts must stop it as well as the native stream;
    // otherwise every refusal adds another permanent 20Hz feed of zero bars.
    complete_start(ready, &alive, &stop)?;

    capture.session = Some(Session {
        stop,
        ack: ack_rx,
        thread,
        samples,
        source_rate,
        alive,
    });
    Ok(())
}

/// Stops recording and returns the whole take as 16kHz mono samples.
///
/// `key_up_ms` is the OS key-up stamp (hotkey::now_ms, wall clock). A
/// quiet release closes the mic at once, exactly as before. If speech was
/// still running into the release, the mic stays open until the word
/// ends, and never past 600ms after key-up (capture/tail.rs). That wait
/// blocks this thread, the same deal the teardown wait below already
/// makes.
pub fn stop(state: &CaptureState, key_up_ms: u64) -> Result<Vec<f32>, String> {
    let session = state
        .lock()
        .unwrap()
        .session
        .take()
        .ok_or("not recording")?;

    // ---- the release tail ----
    // The session is already out of the shared state, so no lock is held
    // while we listen. A press during the tail never reaches start(): the
    // coordinator is in "transcribing" by now, and refuses it.
    let t = std::time::Instant::now();
    let rate = session.source_rate;
    // Swap the take out of the shared buffer (O(1) under the lock). The
    // callback gets a fresh buffer with a second of room, so it never
    // allocates mid-tail.
    let mut recorded = std::mem::replace(
        &mut *session.samples.lock().unwrap(),
        Vec::with_capacity(rate as usize),
    );
    // The one place two clocks meet. key_up_ms is wall clock (SystemTime,
    // stamped in hotkey.rs at the OS event), and so is now_ms(): the same
    // clock read twice, ms apart, so the subtraction is honest. After this
    // line everything is monotonic: an Instant deadline for the mic, and
    // sample counts for the audio. A clock change mid-tail can't stretch
    // the listening.
    let since_ms = tail::since_key_up(key_up_ms, crate::hotkey::now_ms());
    let key_up = tail::key_up_index(recorded.len(), rate, since_ms);
    let deadline = t + std::time::Duration::from_millis(tail::MAX_TAIL_MS.saturating_sub(since_ms));
    let threshold = tail::speech_threshold(&recorded[..key_up], rate);
    let decide = |audio: &[f32]| match threshold {
        Some(threshold) => tail::decide(audio, rate, key_up, threshold),
        // Nothing but digital zeros before key-up: nothing to finish.
        None => tail::Tail::Close {
            ms: 0,
            why: tail::Why::QuietAtRelease,
        },
    };
    // Decide FIRST, sleep only when told to listen. A quiet release pays
    // for one pass over its samples (microseconds) and nothing else.
    let mut verdict = decide(&recorded);
    let decide_us = t.elapsed().as_micros();
    while verdict == tail::Tail::Listen {
        let now = std::time::Instant::now();
        if now >= deadline {
            break; // the clock ran out before the audio did
        }
        std::thread::sleep(TAIL_POLL.min(deadline - now));
        recorded.append(&mut *session.samples.lock().unwrap());
        verdict = decide(&recorded);
    }
    let waited_ms = t.elapsed().as_millis() as u64;
    let (tail_ms, why) = match verdict {
        tail::Tail::Close { ms, why } => (ms, why.describe()),
        // Still listening at the deadline: audio arrived late (input
        // latency, or a stalled device), but the mic closes on time.
        tail::Tail::Listen => (since_ms + waited_ms, tail::Why::Capped.describe()),
    };
    println!(
        "[timing] tail {tail_ms}ms ({why}) · stop arrived {since_ms}ms after key-up · \
         waited {waited_ms}ms · decided in {decide_us}µs"
    );

    session.alive.store(false, Ordering::Relaxed);
    let _ = session.stop.send(());
    // Wait for teardown, but only briefly: a hung device must cost us a
    // zombie thread at worst, never a wedged pipeline. The session was
    // already take()n above, so the next press starts clean regardless.
    let t = std::time::Instant::now();
    match session.ack.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(()) => {
            let _ = session.thread.join(); // ack sent: join returns promptly
        }
        Err(_) => eprintln!("[capture] mic teardown hung; proceeding with buffered audio"),
    }
    let teardown_ms = t.elapsed().as_millis();
    // Plus whatever arrived since the tail's last look.
    recorded.append(&mut *session.samples.lock().unwrap());
    // A duplicate/accidental stop can arrive before ALSA has delivered its
    // first period. Never send an invalid empty WAV to Whisper (where it looks
    // like an engine outage and used to trigger 90 seconds of retries).
    let minimum = (session.source_rate / 20) as usize; // 50ms
    if recorded.len() < minimum {
        return Err(format!(
            "recording ended before the microphone delivered audio ({}/{} samples)",
            recorded.len(),
            minimum
        ));
    }
    // Diagnostic breadcrumb: a "working" mic that delivers silence (seen
    // after a USB replug) shows up here as a near-zero peak on a real take.
    let peak = recorded.iter().fold(0f32, |m, s| m.max(s.abs()));
    println!("[capture] take peak level: {peak:.4}");
    let t = std::time::Instant::now();
    let out = resample(&recorded, session.source_rate, TARGET_SAMPLE_RATE);
    println!(
        "[timing] capture stop: teardown {teardown_ms}ms · resample {}ms ({} samples @{}Hz → {} @{}Hz)",
        t.elapsed().as_millis(),
        recorded.len(),
        session.source_rate,
        out.len(),
        TARGET_SAMPLE_RATE
    );
    Ok(out)
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
    fn failed_start_stops_every_worker_it_launched() {
        let alive = AtomicBool::new(true);
        let (stop, stopped) = channel();

        let error = complete_start(Err("mic never became ready".into()), &alive, &stop)
            .expect_err("failed readiness must stay failed");

        assert_eq!(error, "mic never became ready");
        assert!(!alive.load(Ordering::Relaxed));
        assert!(stopped.try_recv().is_ok());
    }

    #[test]
    fn ready_start_keeps_its_workers_alive() {
        let alive = AtomicBool::new(true);
        let (stop, stopped) = channel();

        complete_start(Ok(()), &alive, &stop).expect("ready stream must stay ready");

        assert!(alive.load(Ordering::Relaxed));
        assert!(stopped.try_recv().is_err());
    }

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
