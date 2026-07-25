//! Stage 3: the model, behind a wall. Wraps samples in a WAV container,
//! POSTs them to the whisper.cpp sidecar over localhost, returns text.
//! Deliberately boring — the model stays a black box on the far side of
//! an HTTP boundary you can point at.

use std::io::Cursor;

pub const SIDECAR_PORT: u16 = 8642;

/// 16kHz mono samples in, whitespace-normalized text out.
pub async fn transcribe(samples: Vec<f32>) -> Result<String, String> {
    let wav = wav_bytes(&samples)?;
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{SIDECAR_PORT}/inference"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("sidecar unreachable: {e}"))?;

    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let text = body["text"].as_str().unwrap_or_default();

    // The server line-breaks mid-sentence; pasting wants a single line.
    Ok(strip_markers(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}

/// Whisper annotates non-speech in brackets — "[BLANK_AUDIO]", "[MUSIC]",
/// "(door slams)" — and those must never be typed into anyone's document.
/// (Discovered when the silence smoke test pasted "[BLANK_AUDIO]" into
/// Notepad.) Dropping bracketed spans means a silent take collapses to the
/// empty string, which the coordinator already knows not to inject.
fn strip_markers(text: &str) -> String {
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
