# The Sidecar — proven 2026-07-25

Step 1 of v1 is done: whisper.cpp's server runs on the RTX 2070 via CUDA and
transcribes correctly. Numbers and procedure below so nobody rediscovers this.

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
4. **Whisper annotates non-speech in brackets** — a silent take transcribes as
   `[BLANK_AUDIO]`, and markers like `[MUSIC]` / `(door slams)` exist too. Discovered
   when the v1 smoke test pasted `[BLANK_AUDIO]` into Notepad. `transcribe.rs` strips
   bracketed spans before returning text; a silent take therefore returns `""` and the
   coordinator skips injection.
