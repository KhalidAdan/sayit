# Building sayit — local dictation for Windows and Linux

> The sections below preserve the original 2026-07-22 scoping notes. The shipped
> implementation is now a single Tauri/Rust codebase: Windows uses hold-F9 and
> SendInput; Linux uses evdev, uinput, Wayland data-control, and a D-Bus
> StatusNotifierItem.

## Current Linux build

The immutable desktop host needs no compiler or CUDA toolkit. Development uses a
Distrobox with `gcc`, `pkg-config`, GTK 3, WebKitGTK 4.1, ALSA development
headers, Rust, and Cargo:

```bash
npm ci
npm run build
distrobox enter sayit-dev -- bash -lc \
  'cd /home/aadan/Development/sayit/src-tauri && cargo test && CARGO_BUILD_JOBS=2 cargo build --release --features custom-protocol'
SAYIT_NO_LAUNCH=1 scripts/install-linux.sh
```

The CUDA companion is built in `nvidia/cuda:12.4.1-devel-ubuntu22.04` for SM
8.6. CUDA runtime and cuBLAS are statically linked into `whisper-server`; NCCL
is disabled because this target has one GPU. The installed NVIDIA driver is its
only GPU dependency. See `.github/workflows/engine-linux.yml` for the
reproducible build.

The release binary remains dynamically linked to system GTK/WebKitGTK/ALSA,
which are desktop runtime libraries already present on the target. First-run
assets are downloaded to the per-user data directory with resume, exact size,
SHA-256, signature verification for the CUDA manifest, and atomic activation.

## Original scope

Verdict up front: **a working version is a weekend project, not a moonshot.**
The AI model is already solved and free; the app is mostly glue.

## Vocabulary

- **STT (speech-to-text)** — what this app does. TTS is the reverse (computer talks).
- **Whisper** — OpenAI's open-source STT model. Runs locally, free, offline. The de facto
  standard; most commercial dictation apps (including WhisperTyping) are built on it.
- **Model weights** — the learned numbers that make a model work. Just a file you download
  (tens to hundreds of MB depending on size).
- **Model sizes** — Whisper ships as `tiny` / `base` / `small` / `medium` / `large`.
  Bigger = more accurate + slower. Accuracy vs. latency is tuned by picking a size.
- **Quantization** — compressing weights to lower precision. Smaller/faster model, tiny
  accuracy cost. "JPEG for neural networks."
- **Inference** — running a model to get output (vs. training one, which we never do).
- **Latency** — delay between finishing speaking and seeing text. The thing that actually
  annoys us about WhisperTyping.
- **Batch transcription** — record everything → stop → transcribe the whole clip → text
  appears at once. What WhisperTyping does. Latency grows with utterance length.
- **Streaming transcription** — text appears while you're still talking (live-caption
  style). Whisper was *not designed for this* — it wants a complete chunk (up to 30s) with
  full context. That's why WhisperTyping doesn't stream.
- **Chunking** — faking streaming by re-transcribing everything-so-far every couple of
  seconds. Works, but text revises itself as the model changes its mind ("I scream" →
  "ice cream").
- **VAD (voice activity detection)** — a tiny cheap model that detects pauses between
  phrases. Transcribe each finished phrase during natural pauses → most text is done by
  the time you stop talking. Best effort-to-payoff route to near-streaming.
- **Streaming-native models** — e.g. NVIDIA Parakeet. Designed to emit words as they hear
  them. Better results, rougher tooling, off the Whisper path.
- **Waveform / audio visualizer** — the bouncing-bars mic graphic. Trivially easy (bar
  height = volume). Mostly a psychological trick: feedback that recording is happening.
- **Local vs. cloud** — model on your machine vs. API over the internet. Local = free,
  private, offline.

## Architecture — four pieces

1. **Global hotkey** — system-wide "hold F9 to talk" (`keyboard` / `pynput`, ~5 lines)
2. **Mic capture** — record while held (`sounddevice`, ~15 lines)
3. **Transcription** — audio → Whisper → text (`faster-whisper`, ~3 lines)
4. **Text injection** — put text on clipboard, simulate Ctrl+V into the focused app (~5 lines)

Total v1: **~60–100 lines of Python.** No API keys, no per-use cost, fully offline.

## Why Python (the "isn't Python slow?" answer)

Python is the wrapping paper, not the gift. `faster-whisper` hands the audio to
**CTranslate2, a C++ engine** (or the GPU via CUDA); Python contributes ~a millisecond of
the ~1 second of work. All of AI is like this — PyTorch/NumPy are C++ engines wearing a
Python interface. Rewriting the glue in Rust makes the app ~0.1% faster.

A systems language matters only for: distribution (single small .exe vs. PyInstaller
bloat), idle memory footprint, or writing the runtime itself (never — use `whisper.cpp`).
Port to C#/Rust/Tauri *after* the concepts are learned; they transfer 1:1.

## Why not the browser / TypeScript

Whisper *can* run in a browser — `whisper.cpp` compiles to **WebAssembly**, **WebGPU**
gives JS the graphics card, and **transformers.js** packages it. Weights download over
HTTP once and cache in IndexedDB. But the **browser sandbox** forbids everything that
makes a dictation app: no global hotkeys, no typing into other apps, no tray, no
background mic. A browser build can only be a destination page you copy text out of.

**Electron** is ruled out (bloat nightmares — every app ships a private Chromium).
**Tauri** is the escape hatch if a TypeScript version ever calls: uses the OS webview
(WebView2, already on Windows 11), ~5–10MB binaries, Rust shell, model as a **sidecar**
process. Several trendy Whisper apps are Tauri apps.

## Roadmap

- **v1** — push-to-talk, batch, pastes into any app. The 100-line weekend version.
- **v2** — tray icon, waveform popup, mic selection, latency tuning (model size / GPU).
- **v3** — VAD-based near-streaming; reassess whether true streaming is even wanted.

Key reframe: what we hate about WhisperTyping is probably **latency, not the absence of
streaming**. A 15-second thought transcribed half a second after key-release doesn't miss
streaming at all.

Honest caveats: 1–3s lag on CPU with small models (NVIDIA GPU changes this dramatically);
first transcription pays a model-load cost.

## Open questions before v1

- Is Python installed?
- What GPU? (NVIDIA → CUDA → dramatically faster inference, bigger models viable)
