//! Runtime companion discovery. Environment overrides win, then the managed
//! per-user data directory, then installed/repository layouts around the
//! executable. No companion path is compiled into the binary.

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
    #[cfg(target_os = "linux")]
    {
        let root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(std::env::temp_dir);
        return root.join("dev.khalid.sayit");
    }
    #[cfg(windows)]
    {
        let root = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        return root.join("sayit");
    }
    #[allow(unreachable_code)]
    std::env::temp_dir().join("sayit")
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
    #[cfg(windows)]
    let candidates = vec![
        PathBuf::from("sidecar/whisper-server.exe"),
        PathBuf::from("sidecar/whisper-cublas/Release/whisper-server.exe"),
    ];
    #[cfg(target_os = "linux")]
    let candidates = vec![
        PathBuf::from("sidecar/whisper-server"),
        PathBuf::from("sidecar/whisper-cuda/whisper-server"),
        PathBuf::from("sidecar/whisper-bin-x64/whisper-server"),
    ];

    #[cfg(windows)]
    let name = "whisper-server.exe";
    #[cfg(target_os = "linux")]
    let name = "whisper-server";

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
