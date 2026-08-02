# The Test Plan

sayit lives or dies on trust: the first take it silently eats is the last
take the user gives it. This plan exists so that never happens. It follows
the pipeline architecture — each stage is tested at the cheapest layer that
can catch its failures — and it grows by one rule: **every bug that reaches
a human becomes a permanent test.**

## Philosophy

- **Test the seams, trust the stages.** The four stages only touch through
  narrow contracts (16kHz mono f32, one-line text, two hotkey events). Test
  the contracts hard and integration mostly takes care of itself.
- **The gap is a correctness property.** Latency regressions are failures,
  not vibes. Once gap instrumentation lands (v2 plank 2), every dictation
  logs a number, and the numbers are the benchmark suite.
- **Silence is the enemy of trust.** Failure modes that make *no sound and
  no text* are the worst class of bug. Tests must assert that every refusal
  and error is audible or visible, never mute.

## Layer 0 — Unit tests (run on every build: `cargo test`)

Pure functions, no devices, milliseconds to run. Implemented now:

| area | cases |
|---|---|
| `capture::resample` | same-rate passthrough; empty input; exact 3:1 (48k→16k) sample alignment; 44.1k→16k length; midpoint interpolation |
| `transcribe::strip_markers` | `[BLANK_AUDIO]` collapses to empty (regression, see ledger); markers dropped while speech survives; plain text untouched; unbalanced brackets can't eat the sentence |
| `transcribe::wav_bytes` | RIFF/WAVE header; exact byte size; out-of-range samples clamp instead of overflowing |

To add when the code arrives: coordinator state-machine transitions (once
Effect lands, the machine becomes pure and property-testable in vitest);
hotkey press/release dedupe (extract the transition to a pure function).

## Layer 1 — Stage contract tests (run when touching a stage)

Each stage exercised alone against its real dependency:

- **Transcribe ↔ sidecar:** the *robot voice test*. Windows TTS synthesizes
  a known sentence to 16kHz WAV; POST it to a running sidecar; assert the
  text round-trips word-for-word. Deterministic, no human, no mic. (This
  test bootstrapped the whole project — see docs/sidecar.md.)
- **Capture:** open the default mic, record 500ms, assert non-empty buffer
  at the contract rate. Can't assert *content* (rooms vary); asserts shape.
- **Inject:** focus a scratch Notepad, inject a marker string, read it back
  via UI automation. The clipboard save/restore is asserted around it.

## Layer 2 — End-to-end smoke (run before calling any change done)

The full loop with synthesized input: `scripts/smoke-input.ps1` opens a
containment Notepad, synthesizes an F9 hold, and the app log is asserted to
show the full sequence (`sound: press` → `captured` → `transcribed`).

**Containment rule (absolute):** any test that can reach the inject stage
MUST focus a scratch window first. The pipeline *works*; working means
pasting into whatever has focus. This rule exists because it was violated
once (see ledger).

Pre-flight for all smoke runs: kill ghost processes (`sayit`,
`whisper-server`, anything on ports 1420/8642) — stale trees from previous
sessions cause false failures.

## Linux release gate — this machine

Before calling Linux complete, perform three consecutive takes in **Ghostty,
Helium, and Zed** and verify all of the following:

- double-tap left Control starts; one clean tap stops; ordinary Ctrl shortcuts
  and typing never trigger sayit;
- text lands in the focused application and both Clipboard and Primary are
  restored after every take;
- an 8-second warm take has a felt gap of at most 1 second;
- keyboard and microphone replug recover without restarting sayit;
- suspend/resume restores hotkey, microphone, tray truth, and inference;
- login autostart works; the tray Diagnostics item reopens setup;
- a signed binary update swaps atomically, and a candidate that fails before
  healthy startup rolls back to `.sayit.previous` on the next launch.

The setup window's **Test typing** control is the containment target for direct
injection checks. Never run a synthetic gesture unless a scratch input is
focused first. Virtual uinput keyboards do not receive the active-session ACL
that physical `event*` devices do on this host, so the evdev gesture gate is a
manual physical-key test rather than a misleading synthetic one.

## Layer 3 — Perceptual checks (run per version, human required)

The things only Khalid's ears and hands can judge:

- **The eight-second demo:** cursor in an app never configured, hold, speak
  naturally (backtracking and "um"s included), release. Words correct,
  gap unnoticeable, nothing extra typed.
- **Punctuation & capitalization** look like writing, not transcript soup.
- **Sound audition:** each soundpack slot audible at normal volume, press
  sound doesn't bleed into the transcription (dictate with sounds on, check
  the text for phantom words).
- **Tray truth:** tray status tracks reality through a full dictation.
- **The trust question**, asked honestly: "did I hesitate before using it
  for something that mattered?" If yes, find out why — that's the next bug.

## The Regression Ledger

Every one of these reached a human before a test caught it. Each is now
pinned by the layer named. Each carries its episode title — *The Dream
Team in:* — because memorable bugs teach lessons that stick.

| episode | bug | lesson | pinned at |
|---|---|---|---|
| **The Phantom Transcript** | `[BLANK_AUDIO]` pasted into Notepad | Whisper annotates non-speech in brackets | Layer 0 (`strip_markers`) |
| **The Containment Breach** | Smoke test pasted live room audio into an unknown focused window | the pipeline working = pasting; containment is mandatory | Layer 2 (containment rule) |
| **The Invisible Byte** | PowerShell 5.1 wrote BOM into tauri.conf.json; app wouldn't boot | `Set-Content -Encoding utf8` is not UTF-8 | build docs; avoid PS for config writes |
| **The Memory Gremlin** | rustc crashed (STATUS_STACK_BUFFER_OVERRUN) on 16GB RAM | full-parallel builds exhaust memory; use `-j 2` | build docs |
| **The Gremlin Returns** | release build OOM'd even at `-j 2` (LLVM out of memory on the `windows` crate) | opt-level=3 multiplies per-crate memory; release builds want `-j 1` with the dev app and sidecar closed | build docs |
| **The Ghosts of Port 1420** | Ghost vite/sidecar from a dead session blocked relaunch | dev processes outlive sessions on Windows | Layer 2 pre-flight |
| **The Fifty-Five Second Toll** | First CUDA inference costs ~55s | warmup at boot is mandatory, and is the readiness probe | design (sidecar.rs) + Layer 1 |
| **The Unheard Announcement** | `sidecar_ready` event fired before webview listeners existed (driver-cached CUDA kernels make later warmups fast); every press refused | push-only readiness is a race — state must also be pullable (`is_ready`) | design (sidecar::Ready) + Layer 2 smoke asserts a press is accepted |
| **The Unplugging** | USB mic replug hung a WASAPI stream; unbounded thread join wedged the coordinator forever ("app does not work") | never wait unboundedly at a seam: handshake with timeout on stream start, bounded ack on teardown, Effect.timeout as the coordinator's seatbelt | design (capture.rs handshakes, main.ts timeout) + Layer 3: replug the mic mid-session, verify next take works |
| **The Deadlock Hunt** | Second press after any completed take hung silently: the sleep-timer fiber was forked inside `ensuring()` (a finalizer = uninterruptible region), inherited uninterruptibility, and `Fiber.interrupt` awaited the full 10-minute sleep | fibers forked in finalizers inherit uninterruptible — mark them `Effect.interruptible` AND cancel with `Fiber.interruptFork` (never await a fiber's death on the hot path). Diagnosed by layered probes: hotkey event log → heartbeat → callback probe | design (main.ts) + Layer 2: smoke runs THREE consecutive takes with the engine ready — single-take smoke let this hide for a day |
| **The Tap That Kept Listening** | A sub-100ms tap delivered `push_finished` while `start_capture` was still in flight; the `!== "recording"` guard dropped the release and the mic recorded for 77s until the next tap — which then pasted the entire ambient transcript | between two async stages, an event that arrives "too early" is state, not noise: park it and replay it when the stage completes (`pressInFlightRef` / `releasedDuringStartRef`) | design (main.ts) + Layer 2: smoke adds a quick-tap (~30ms hold) and asserts capture stops without a second press |

## What is deliberately not tested

- Whisper's accuracy itself — the model is a black box behind a contract;
  we test the contract (robot voice round-trip), not the model's brain.
- Audio playback correctness (does the woosh *sound* right) — perceptual,
  Layer 3 only.
- Cross-machine portability — v1/v2 target this machine, per the north
  star; the installer-for-strangers milestone reopens this.
