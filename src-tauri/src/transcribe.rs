//! Stage 3: the model, behind a wall. Wraps samples in a WAV container,
//! POSTs them to the whisper.cpp sidecar over localhost, returns text.
//! Deliberately boring — the model stays a black box on the far side of
//! an HTTP boundary you can point at.

use std::io::Cursor;
use std::time::{Duration, Instant};

pub const SIDECAR_PORT: u16 = 8642;

/// Where this stage's milliseconds went. Filled by `transcribe`, extended
/// with the engine wait by `transcribe_waiting`, and carried all the way up
/// to the coordinator so gap-log.csv can tell inference apart from plumbing.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Timing {
    /// Spent retrying while the engine was still waking (includes the WAV
    /// encodes of failed attempts). 0 on a warm take.
    pub engine_wait_ms: u64,
    /// Attempts made. 1 = the engine was warm and answered first try.
    pub attempts: u32,
    /// Encoding samples into an in-memory WAV.
    pub wav_ms: u64,
    /// POST to localhost → full response body received. This IS whisper
    /// inference; localhost HTTP overhead is too small to see next to it.
    pub http_ms: u64,
    /// JSON field extraction + marker stripping + whitespace normalization.
    pub parse_ms: u64,
}

/// Like `transcribe`, but patient: while the engine is waking (connection
/// refused), retry every 500ms until the deadline. The user's take is
/// often the very first request after a wake — audio must never be lost
/// to a nap the app itself decided to take.
pub async fn transcribe_waiting(
    samples: Vec<f32>,
    deadline: Duration,
) -> Result<(String, Timing), String> {
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        // Everything before the attempt that succeeds is "engine wait".
        let waited_ms = started.elapsed().as_millis() as u64;
        match transcribe(samples.clone()).await {
            Ok((text, mut timing)) => {
                timing.engine_wait_ms = waited_ms;
                timing.attempts = attempts;
                if attempts > 1 {
                    println!("[timing] engine wait: {waited_ms}ms across {attempts} attempts");
                }
                return Ok((text, timing));
            }
            Err(e) if started.elapsed() < deadline => {
                let _ = e; // engine still waking; keep trying
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(format!("engine never answered: {e}")),
        }
    }
}

/// 16kHz mono samples in, whitespace-normalized text out — plus where the
/// time went.
pub async fn transcribe(samples: Vec<f32>) -> Result<(String, Timing), String> {
    let mut timing = Timing::default();

    let t = Instant::now();
    let wav = wav_bytes(&samples)?;
    timing.wav_ms = t.elapsed().as_millis() as u64;
    let wav_kb = wav.len() / 1024;

    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let t = Instant::now();
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{SIDECAR_PORT}/inference"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("sidecar unreachable: {e}"))?;
    // Reading the body is still the network; it belongs to http_ms.
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    timing.http_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let text = body["text"].as_str().unwrap_or_default();
    // The server line-breaks mid-sentence; pasting wants a single line.
    let text = strip_markers(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    timing.parse_ms = t.elapsed().as_millis() as u64;

    println!(
        "[timing] transcribe: wav {}ms · inference(http) {}ms · parse {}ms ({wav_kb} KB wav)",
        timing.wav_ms, timing.http_ms, timing.parse_ms
    );
    Ok((text, timing))
}

/// Whisper annotates non-speech in brackets — "[BLANK_AUDIO]", "[MUSIC]",
/// "(door slams)" — and those must never be typed into anyone's document.
/// (Discovered when the silence smoke test pasted "[BLANK_AUDIO]" into
/// Notepad.) Dropping bracketed spans means a silent take collapses to the
/// empty string, which the coordinator already knows not to inject.
pub(crate) fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 16-bit PCM WAV, built in memory.
pub fn wav_bytes(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::capture::TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        let as_int = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(as_int).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: the v1 smoke test pasted "[BLANK_AUDIO]" into Notepad.
    // A silent take must collapse to nothing so the coordinator skips it.
    #[test]
    fn blank_audio_marker_collapses_to_empty() {
        assert_eq!(strip_markers("[BLANK_AUDIO]").trim(), "");
    }

    #[test]
    fn markers_are_dropped_but_speech_survives() {
        assert_eq!(
            strip_markers("(door slams) hello there [MUSIC] friend").split_whitespace().collect::<Vec<_>>(),
            vec!["hello", "there", "friend"]
        );
    }

    #[test]
    fn plain_speech_is_untouched() {
        assert_eq!(strip_markers("no markers here"), "no markers here");
    }

    #[test]
    fn unbalanced_close_bracket_does_not_eat_text() {
        // saturating_sub means a stray "]" can't push depth negative and
        // swallow the rest of the sentence.
        assert_eq!(strip_markers("] still here"), "] still here".replace(']', ""));
    }

    #[test]
    fn wav_bytes_has_riff_header_and_correct_size() {
        let wav = wav_bytes(&[0.0f32; 160]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        // 44-byte canonical header + 2 bytes per 16-bit sample.
        assert_eq!(wav.len(), 44 + 160 * 2);
    }

    #[test]
    fn wav_bytes_clamps_out_of_range_samples() {
        // A sample above 1.0 must clamp, not overflow into garbage.
        let wav = wav_bytes(&[2.0f32]).unwrap();
        let sample = i16::from_le_bytes([wav[44], wav[45]]);
        assert_eq!(sample, i16::MAX);
    }
}
