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
}
