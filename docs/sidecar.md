# The Sidecar — proven on Windows and Linux

## Linux proof — 2026-08-02

`whisper-server` v1.9.1 is compiled in an NVIDIA CUDA 12.4 container for SM
8.6. It is one 653 MB executable with CUDA runtime and cuBLAS linked in; `ldd`
shows no CUDA toolkit or NCCL dependency, only the normal NVIDIA driver
`libcuda.so.1` and baseline system C/C++ libraries.

On this machine's RTX 3070, the server loaded `ggml-small.bin` and reported its
CUDA backend ready in 3 seconds. Two 0.5-second silence inference requests took
112 ms and 43 ms. The first-run setup performs the same inference as warmup.
Released apps select this engine through a signed companion manifest, with the
pinned official Ubuntu CPU archive as a functional fallback.

```bash
~/.local/share/dev.khalid.sayit/engine/whisper-server \
  -m ~/.local/share/dev.khalid.sayit/models/ggml-small.bin \
  --host 127.0.0.1 --port 8642
```

## Windows proof — 2026-07-25

Step 1 of v1 proved whisper.cpp's server on the RTX 2070 via CUDA. Numbers and
procedure remain below so nobody rediscovers this.

## What's installed

- `sidecar/whisper-cublas/Release/whisper-server.exe` — whisper.cpp **v1.9.1**, CUDA
  12.4 build (`whisper-cublas-12.4.0-bin-x64.zip` from the ggml-org/whisper.cpp GitHub
  release, 646 MB). Driver 595.97 / CUDA 13.2 runs it fine.
- `models/ggml-small.bin` — Whisper `small`, 465 MB, from the official
  ggerganov/whisper.cpp Hugging Face repo. Loads as 487 MB in VRAM.
- The same zip also ships `whisper-stream.exe`, VAD test tools, and Parakeet binaries —
  v3 material, already on disk.

## How to run it

```
sidecar\whisper-cublas\Release\whisper-server.exe -m models\ggml-small.bin --host 127.0.0.1 --port 8642
```

Transcribe: `POST http://127.0.0.1:8642/inference`, multipart field `file` = a
**16kHz mono WAV**, `response_format=json` → `{"text": "..."}`.

## Measured numbers (8-second spoken test clip)

| call | round trip |
|---|---|
| first after boot | **54.9 s** — one-time CUDA warmup (kernel/graph init) |
| second | 658 ms |
| third | 432 ms |

Transcription was word-perfect on all three, including the made-up phrase
"say it sidecar".

## Design consequences

1. **Warmup is mandatory at app start.** The app must fire a throwaway inference
   (silence is fine) the moment the sidecar boots, so the user never pays the 55-second
   first-call cost. This is now a v1 requirement, not a v2 nicety.
2. **The warm gap is ~0.5 s for 8 s of speech.** Under the north star's threshold —
   batch is comfortably viable; no streaming pressure yet.
3. Server startup (model → VRAM) takes a few seconds; the app should treat "sidecar
   ready" as an event, not an assumption. Warmup doubles as the readiness probe.
4. **The engine has a metabolism** (added with the hybrid sleep/wake
   feature): after 10 idle minutes the coordinator kills whisper-server,
   freeing ~500MB VRAM; the tray reads "engine sleeping — press to talk."
   A press from sleep ALWAYS records — capture needs no engine — while
   `engine_start` wakes it concurrently and `transcribe_waiting` retries
   until warm (seconds, thanks to driver-cached CUDA kernels). "Keep
   engine awake" in the tray pins it hot. The key always works; the user
   manages nothing.
5. **Whisper annotates non-speech in brackets** — a silent take transcribes as
   `[BLANK_AUDIO]`, and markers like `[MUSIC]` / `(door slams)` exist too. Discovered
   when the v1 smoke test pasted `[BLANK_AUDIO]` into Notepad. `transcribe.rs` strips
   bracketed spans before returning text; a silent take therefore returns `""` and the
   coordinator skips injection.
