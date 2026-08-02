//! First-run companion bootstrap. Downloads are restartable, checksummed,
//! and become visible to the runtime only through an atomic rename.

use base64::Engine as _;
use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use reqwest::header::RANGE;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

const ENGINE_URL: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-bin-ubuntu-x64.tar.gz";
const ENGINE_SHA256: &str = "f3bf3b4369a99b54665b0f19b88483b30de27f25963b0414235dea03198515c5";
const ENGINE_SIZE: u64 = 9_379_235;
const CUDA_MANIFEST_URL: &str = "https://github.com/KhalidAdan/sayit/releases/download/engine-v1.9.1-cuda12.4/engine-linux-x86_64.json";
const CUDA_MANIFEST_SIG_URL: &str = "https://github.com/KhalidAdan/sayit/releases/download/engine-v1.9.1-cuda12.4/engine-linux-x86_64.json.sig";
const UPDATE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI1QjFFMkUzQzJCQ0E2MApSV1JneWlzOExoNWJBbndKMzBOMXdCbmZIS3BaMDhsK0ozOFdOeklsZlBnb1NDSEVlbXo0elhUSgo=";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin";
const MODEL_SHA256: &str = "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b";
const MODEL_SIZE: u64 = 487_601_967;

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineSpec {
    backend: String,
    url: String,
    size: u64,
    sha256: String,
    archive_root: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    asset: &'a str,
    downloaded: u64,
    total: u64,
    phase: &'a str,
}

fn verify_signed(data: &[u8], signature_b64: &str) -> Result<(), String> {
    let public_text = base64::engine::general_purpose::STANDARD
        .decode(UPDATE_PUBKEY)
        .map_err(|e| e.to_string())?;
    let public_text = std::str::from_utf8(&public_text).map_err(|e| e.to_string())?;
    let signature_text = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .map_err(|e| e.to_string())?;
    let signature_text = std::str::from_utf8(&signature_text).map_err(|e| e.to_string())?;
    let public = PublicKey::decode(public_text).map_err(|e| e.to_string())?;
    let signature = Signature::decode(signature_text).map_err(|e| e.to_string())?;
    public
        .verify(data, &signature, true)
        .map_err(|e| format!("engine manifest signature failed: {e}"))
}

async fn engine_spec() -> EngineSpec {
    let client = match reqwest::Client::builder()
        .user_agent(concat!("sayit/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => return cpu_spec(),
    };
    let fetched = async {
        let manifest = client
            .get(CUDA_MANIFEST_URL)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;
        let signature = client
            .get(CUDA_MANIFEST_SIG_URL)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        verify_signed(&manifest, &signature)?;
        serde_json::from_slice::<EngineSpec>(&manifest).map_err(|e| e.to_string())
    }
    .await;
    match fetched {
        Ok(spec) if spec.backend == "cuda-12.4-sm86" => spec,
        Ok(_) => cpu_spec(),
        Err(e) => {
            println!("[setup] CUDA engine manifest unavailable; using CPU fallback ({e})");
            cpu_spec()
        }
    }
}

fn cpu_spec() -> EngineSpec {
    EngineSpec {
        backend: "cpu-fallback".into(),
        url: ENGINE_URL.into(),
        size: ENGINE_SIZE,
        sha256: ENGINE_SHA256.into(),
        archive_root: "whisper-bin-ubuntu-x64".into(),
    }
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

async fn download(
    app: &AppHandle,
    name: &str,
    url: &str,
    expected_size: u64,
    expected_sha: &str,
    destination: &Path,
) -> Result<(), String> {
    if destination.exists()
        && destination.metadata().map(|m| m.len()).unwrap_or(0) == expected_size
        && sha256(destination)? == expected_sha
    {
        let _ = app.emit(
            "setup_progress",
            Progress {
                asset: name,
                downloaded: expected_size,
                total: expected_size,
                phase: "ready",
            },
        );
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = destination.with_extension("part");
    let mut offset = part.metadata().map(|m| m.len()).unwrap_or(0);
    if offset > expected_size {
        let _ = fs::remove_file(&part);
        offset = 0;
    }

    let client = reqwest::Client::builder()
        .user_agent(concat!("sayit/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !response.status().is_success() {
        return Err(format!("{name} download returned {}", response.status()));
    }
    if !append {
        offset = 0;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .map_err(|e| e.to_string())?;
    let mut downloaded = offset;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > expected_size {
            let _ = fs::remove_file(&part);
            return Err(format!("{name} download exceeded expected size"));
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        let _ = app.emit(
            "setup_progress",
            Progress {
                asset: name,
                downloaded,
                total: expected_size,
                phase: "downloading",
            },
        );
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);

    if downloaded != expected_size {
        return Err(format!(
            "{name} download is incomplete ({downloaded}/{expected_size} bytes)"
        ));
    }
    let _ = app.emit(
        "setup_progress",
        Progress {
            asset: name,
            downloaded,
            total: expected_size,
            phase: "verifying",
        },
    );
    let actual = sha256(&part)?;
    if actual != expected_sha {
        let _ = fs::remove_file(&part);
        return Err(format!("{name} checksum mismatch"));
    }
    fs::rename(&part, destination).map_err(|e| e.to_string())?;
    Ok(())
}

fn install_engine(
    archive_path: &Path,
    destination: &Path,
    archive_root: &str,
    backend: &str,
) -> Result<(), String> {
    let installed_backend =
        fs::read_to_string(destination.join("sayit-backend")).unwrap_or_default();
    if destination.join("whisper-server").exists() && installed_backend.trim() == backend {
        return Ok(());
    }
    let root = destination
        .parent()
        .ok_or("engine destination has no parent")?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let staging = root.join("engine.new");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let gz = flate2::read::GzDecoder::new(File::open(archive_path).map_err(|e| e.to_string())?);
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(&staging)
        .map_err(|e| format!("engine archive is invalid: {e}"))?;
    let extracted = staging.join(archive_root);
    if !extracted.join("whisper-server").is_file() {
        let _ = fs::remove_dir_all(&staging);
        return Err("engine archive did not contain whisper-server".into());
    }
    let mut permissions = fs::metadata(extracted.join("whisper-server"))
        .map_err(|e| e.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(extracted.join("whisper-server"), permissions)
        .map_err(|e| e.to_string())?;
    fs::write(extracted.join("sayit-backend"), format!("{backend}\n"))
        .map_err(|e| e.to_string())?;

    let old = root.join("engine.old");
    let _ = fs::remove_dir_all(&old);
    if destination.exists() {
        fs::rename(destination, &old).map_err(|e| e.to_string())?;
    }
    if let Err(e) = fs::rename(&extracted, destination) {
        if old.exists() {
            let _ = fs::rename(&old, destination);
        }
        return Err(e.to_string());
    }
    let _ = fs::remove_dir_all(&old);
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

fn copy_local_soundpack(managed: &Path) {
    let destination = managed.join("soundpack");
    if destination.exists() {
        return;
    }
    let Some(source) = crate::paths::soundpack_dir() else {
        return;
    };
    if source == destination {
        return;
    }
    if fs::create_dir_all(&destination).is_err() {
        return;
    }
    for slot in [
        "press.mp3",
        "press.ogg",
        "press.wav",
        "refuse.ogg",
        "refuse.wav",
    ] {
        let from = source.join(slot);
        if from.exists() {
            let _ = fs::copy(from, destination.join(slot));
        }
    }
}

pub async fn ensure(app: &AppHandle) -> Result<(), String> {
    let managed = crate::paths::managed_dir();
    fs::create_dir_all(&managed).map_err(|e| e.to_string())?;
    copy_local_soundpack(&managed);

    let downloads = managed.join("downloads");
    if crate::paths::sidecar_exe().is_err() {
        let engine = engine_spec().await;
        let archive = downloads.join(format!("whisper-{}.tar.gz", engine.backend));
        download(
            app,
            "engine",
            &engine.url,
            engine.size,
            &engine.sha256,
            &archive,
        )
        .await?;
        install_engine(
            &archive,
            &managed.join("engine"),
            &engine.archive_root,
            &engine.backend,
        )?;
    }

    download(
        app,
        "model",
        MODEL_URL,
        MODEL_SIZE,
        MODEL_SHA256,
        &managed.join("models/ggml-small.bin"),
    )
    .await?;
    Ok(())
}

pub fn assets_ready() -> bool {
    crate::paths::sidecar_exe().is_ok() && crate::paths::model().is_ok()
}

pub fn paths() -> (PathBuf, PathBuf) {
    (
        crate::paths::managed_dir().join("engine/whisper-server"),
        crate::paths::managed_dir().join("models/ggml-small.bin"),
    )
}
