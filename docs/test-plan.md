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
pinned by the layer named:

| bug | lesson | pinned at |
|---|---|---|
| `[BLANK_AUDIO]` pasted into Notepad | Whisper annotates non-speech in brackets | Layer 0 (`strip_markers`) |
| Smoke test pasted live room audio into an unknown focused window | the pipeline working = pasting; containment is mandatory | Layer 2 (containment rule) |
| PowerShell 5.1 wrote BOM into tauri.conf.json; app wouldn't boot | `Set-Content -Encoding utf8` is not UTF-8 | build docs; avoid PS for config writes |
| rustc crashed (STATUS_STACK_BUFFER_OVERRUN) on 16GB RAM | full-parallel builds exhaust memory; use `-j 2` | build docs |
| Ghost vite/sidecar from a dead session blocked relaunch | dev processes outlive sessions on Windows | Layer 2 pre-flight |
| First CUDA inference costs ~55s | warmup at boot is mandatory, and is the readiness probe | design (sidecar.rs) + Layer 1 |

## What is deliberately not tested

- Whisper's accuracy itself — the model is a black box behind a contract;
  we test the contract (robot voice round-trip), not the model's brain.
- Audio playback correctness (does the woosh *sound* right) — perceptual,
  Layer 3 only.
- Cross-machine portability — v1/v2 target this machine, per the north
  star; the installer-for-strangers milestone reopens this.
