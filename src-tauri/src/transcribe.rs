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
