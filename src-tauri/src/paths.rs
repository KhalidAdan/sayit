//! Runtime companion discovery. Environment overrides win, then the managed
//! per-user data directory, then installed/repository layouts around the
//! executable. No companion path is compiled into the binary; the per-OS
//! facts (data-dir root, exe name, candidate layouts) come from the
//! platform backend as data.

use crate::platform::Platform;
use std::path::{Path, PathBuf};

fn find_from(start: &Path, candidates: &[PathBuf]) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for rel in candidates {
            let candidate = d.join(rel);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        dir = d.parent();
    }
    None
}

pub fn managed_dir() -> PathBuf {
    if let Ok(path) = std::env::var("SAYIT_DATA_DIR") {
        return PathBuf::from(path);
    }
    crate::platform::Current::data_dir()
}

fn find(env_key: &str, managed: PathBuf, candidates: &[PathBuf]) -> Option<PathBuf> {
    if let Ok(overridden) = std::env::var(env_key) {
        return Some(PathBuf::from(overridden));
    }
    if managed.exists() {
        return Some(managed);
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    find_from(&exe_dir, candidates)
}

pub fn sidecar_exe() -> Result<PathBuf, String> {
    let candidates: Vec<PathBuf> = crate::platform::Current::sidecar_candidates()
        .iter()
        .map(PathBuf::from)
        .collect();
    let name = crate::platform::Current::sidecar_exe_name();

    find(
        "SAYIT_SIDECAR",
        managed_dir().join("engine").join(name),
        &candidates,
    )
    .ok_or_else(|| {
        format!(
            "{name} not found — run sayit setup or set SAYIT_SIDECAR (expected under {})",
            managed_dir().display()
        )
    })
}

pub fn model() -> Result<PathBuf, String> {
    find(
        "SAYIT_MODEL",
        managed_dir().join("models/ggml-small.bin"),
        &[PathBuf::from("models/ggml-small.bin")],
    )
    .ok_or_else(|| {
        format!(
            "ggml-small.bin not found — run sayit setup or set SAYIT_MODEL (expected under {})",
            managed_dir().display()
        )
    })
}

/// The Silero VAD weights for whisper-server's `--vad` (sidecar.rs): the
/// exact file the silence filter was tuned and measured against, pinned by
/// name and byte size. Official copy: huggingface.co/ggml-org/whisper-vad.
pub const VAD_MODEL_FILE: &str = "ggml-silero-v5.1.2.bin";
pub const VAD_MODEL_SIZE: u64 = 885_098;

/// Optional, unlike the main model: an Err here means "run the engine
/// without VAD," never "can't run." Searched like the model (SAYIT_VAD_MODEL,
/// the managed dir, the repo layout), then one more place: right beside
/// whichever ggml-small.bin was found. A SAYIT_MODEL that points into a
/// folder of models therefore finds its neighbor with no second variable.
pub fn vad_model() -> Result<PathBuf, String> {
    let rel = PathBuf::from("models").join(VAD_MODEL_FILE);
    let found = find("SAYIT_VAD_MODEL", managed_dir().join(&rel), &[rel])
        // An override naming a missing file falls through to the search,
        // rather than handing whisper-server a path it can't open.
        .filter(|p| p.exists())
        .or_else(|| {
            let beside = model().ok()?.with_file_name(VAD_MODEL_FILE);
            beside.exists().then_some(beside)
        })
        .ok_or_else(|| {
            format!(
                "{VAD_MODEL_FILE} not found (put it beside ggml-small.bin or set SAYIT_VAD_MODEL)"
            )
        })?;
    usable_vad(found)
}

/// A VAD file of the wrong size is refused. This isn't fussiness:
/// whisper-server starts fine with a truncated VAD model, then answers
/// EVERY request with HTTP 500 (seen while testing this) — a half-downloaded
/// nicety would kill the key. Without the flags, the engine runs as before.
fn usable_vad(path: PathBuf) -> Result<PathBuf, String> {
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size != VAD_MODEL_SIZE {
        return Err(format!(
            "{} is {size} bytes, expected {VAD_MODEL_SIZE} (truncated or a different version)",
            path.display()
        ));
    }
    Ok(path)
}

/// Missing soundpack is not an error — every slot is simply silent.
pub fn soundpack_dir() -> Option<PathBuf> {
    find(
        "SAYIT_SOUNDPACK",
        managed_dir().join("soundpack"),
        &[PathBuf::from("soundpack")],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_candidate_in_an_ancestor() {
        let root = std::env::temp_dir().join("sayit-paths-test");
        let deep = root.join("target").join("release");
        std::fs::create_dir_all(root.join("stuff")).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join("stuff").join("thing.bin"), b"x").unwrap();

        let found = find_from(&deep, &[PathBuf::from("stuff/thing.bin")]);
        assert_eq!(found, Some(root.join("stuff").join("thing.bin")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn returns_none_when_nothing_exists() {
        let lonely = std::env::temp_dir().join("sayit-paths-empty");
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(
            find_from(&lonely, &[PathBuf::from("definitely-not-real.xyz")]),
            None
        );
        let _ = std::fs::remove_dir_all(&lonely);
    }

    #[test]
    fn a_vad_file_of_the_wrong_size_is_refused() {
        // A truncated download must mean "no VAD," not "every request fails."
        let dir = std::env::temp_dir().join("sayit-paths-vad");
        std::fs::create_dir_all(&dir).unwrap();
        let truncated = dir.join("truncated.bin");
        std::fs::write(&truncated, vec![0u8; 4096]).unwrap();
        assert!(usable_vad(truncated).is_err());

        let whole = dir.join("whole.bin");
        std::fs::write(&whole, vec![0u8; VAD_MODEL_SIZE as usize]).unwrap();
        assert_eq!(usable_vad(whole.clone()), Ok(whole));

        assert!(usable_vad(dir.join("missing.bin")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
