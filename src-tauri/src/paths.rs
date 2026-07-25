//! Where sayit's companions live — resolved at RUNTIME, so the same exe
//! works from the repo, from a CI artifact, or from an installer. Nothing
//! is compiled in. Resolution order, per companion:
//!
//! 1. Env var override (SAYIT_SIDECAR / SAYIT_MODEL / SAYIT_SOUNDPACK)
//! 2. The installed layout: next to sayit.exe
//! 3. The repo layout: walk up from the exe (target\debug or
//!    target\release are inside the repo) until a candidate exists
//!
//! A CI-built exe dropped anywhere inside a clone finds everything; a
//! bundled app ships its companions beside the binary; dev never thinks
//! about any of this.

use std::path::{Path, PathBuf};

/// Walk from `start` up through its ancestors, returning the first
/// candidate that exists. Pure enough to unit test.
fn find_from(start: &Path, candidates: &[&str]) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for rel in candidates {
            let c = d.join(rel);
            if c.exists() {
                return Some(c);
            }
        }
        dir = d.parent();
    }
    None
}

fn find(env_key: &str, candidates: &[&str]) -> Option<PathBuf> {
    if let Ok(overridden) = std::env::var(env_key) {
        return Some(PathBuf::from(overridden));
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    find_from(&exe_dir, candidates)
}

pub fn sidecar_exe() -> Result<PathBuf, String> {
    find(
        "SAYIT_SIDECAR",
        &[
            r"sidecar\whisper-server.exe",
            r"sidecar\whisper-cublas\Release\whisper-server.exe",
        ],
    )
    .ok_or_else(|| {
        "whisper-server.exe not found — set SAYIT_SIDECAR or place sidecar\\ next to sayit.exe (docs/sidecar.md)".into()
    })
}

pub fn model() -> Result<PathBuf, String> {
    find("SAYIT_MODEL", &[r"models\ggml-small.bin"]).ok_or_else(|| {
        "ggml-small.bin not found — set SAYIT_MODEL or place models\\ next to sayit.exe (docs/sidecar.md)".into()
    })
}

/// Missing soundpack is not an error — every slot is simply silent.
pub fn soundpack_dir() -> Option<PathBuf> {
    find("SAYIT_SOUNDPACK", &["soundpack"])
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

        let found = find_from(&deep, &[r"stuff\thing.bin"]);
        assert_eq!(found, Some(root.join("stuff").join("thing.bin")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn returns_none_when_nothing_exists() {
        let lonely = std::env::temp_dir().join("sayit-paths-empty");
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(find_from(&lonely, &["definitely-not-real.xyz"]), None);
        let _ = std::fs::remove_dir_all(&lonely);
    }
}
