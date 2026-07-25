# sayit

**The key that listens.**

Your keyboard has a key for everything except the way you actually think.

---

## The Idea

sayit is not an app. It has no window, no dock icon, no place you go. It is one new key
on your keyboard: hold it, talk, let go. What you said is typed wherever your cursor
already was, in whatever program you were already in.

That framing is the whole design. A keyboard key sets a brutal standard, and every
decision in this project is downstream of taking that standard literally:

- A key works in every program, without any program knowing about it.
- A key responds the instant you press it. There is no loading state on the letter T.
- A key has no account, no settings screen, no onboarding.
- A key that sent your keystrokes to a server would be called a keylogger. A key that
  hears is still a key.

Speech is the fastest thing a human does with words — three to four times faster than
typing, and closer to how thought actually arrives: half-formed, backtracking, alive.
The keyboard forces you to serialize that. sayit's bet is that the moment speaking into
the computer is as reliable as pressing T, you'll stop noticing which one you're doing.

## What the Key Owes You

**The gap is the enemy.** The gap: you let go of the key, and words haven't appeared
yet. Every version of sayit is judged by that gap, measured in milliseconds, not
adjectives. Features that widen it don't get built; features that don't shrink it wait.

**Everywhere, or it doesn't count.** Dictation that works in some apps is a demo. The
terminal, the browser, the chat box, the commit message — if the cursor blinks there,
the key works there. Anything less and you're back to checking which tool you're in,
which is exactly the thinking-about-the-tool tax sayit exists to delete.

**Your voice stays home.** Audio is captured, transcribed on this machine, and gone.
No network calls, no API keys, no account, no bill that scales with how much you think
out loud. This isn't a privacy feature to toggle — it's what makes it a key instead of
a service.

**Nothing I can't explain.** sayit is also how I learn this field. The model may stay a
black box; the plumbing may not. Any line of glue I can't teach back to someone gets
rewritten until I can. A tool this close to my keystrokes should have no mysteries in
it — and neither should its author.

## Under the Keycap

```
hotkey ── press ──▶ capture ── audio ──▶ transcribe ── text ──▶ inject
  (global-shortcut)    (cpal)             (whisper.cpp sidecar)   (clipboard + enigo)
```

Four stages, one direction. The audio flows forward and nothing flows back. Each stage
can be ripped out and replaced without the others noticing — swap the model, keep the
plumbing; redraw the UI, keep the pipeline. The day two stages need to know each
other's internals is the day the design has failed and gets rethought.

The house is Tauri: the pipeline lives on the Rust side, where the OS is; TypeScript
(with Effect, from v2 on) owns orchestration and everything the user sees. The model
runs as a sidecar — a bundled whisper.cpp process we talk to, never link against. The
model stays a black box behind a boundary you can point at.

## The Road

### v1 — A key that works

The smallest thing that types one honestly-spoken sentence into a real app. Hold,
speak, release, words. Four pipeline stages in Rust, one sidecar model, a thin
TypeScript coordinator — every line of glue readable in one sitting, even the Rust,
especially the Rust.

v1 deliberately has no interface at all — no tray icon, no waveform, no config, no mic
picker. A prototype key doesn't need a keycap. It needs to close the circuit.

### v2 — A key you forget is software

The version that runs from startup and earns a permanent place under your left hand.
Tray presence, so it can be quit and trusted. The waveform pulse while you hold — the
one piece of UI a key is allowed, because it answers "is this thing hearing me?" without
you looking. Mic selection. Model warm at boot so the first press of the day is as fast
as the hundredth. The gap measured, tuned, and pinned to the GPU if one exists.

v2 begins only after v1 has taken real dictation for real work. The friction list must
be earned by talking, not imagined in advance.

### v3 — A key that keeps up with you

Closing the gap below what batch can do: voice activity detection, transcribing each
phrase in your natural pauses, so the text is mostly finished before you finish. Words
that revise themselves as context arrives — handled gracefully or not at all.

v3 begins only when v2's gap is measured, tuned, and *still* the thing that stings —
a number on the table, not a mood.

### Beyond the road (noticed, not promised)

Each of these waits for the same three-part test: the need showed up while dictating,
the gap doesn't widen, and I can explain the mechanism after building it.

- Streaming-native models (Parakeet and kin) — true live captions, off the Whisper path
- Spoken commands — "new paragraph," "scratch that"
- An LLM pass between transcribe and inject — rambling in, intent out
- An installer for strangers, if sayit ever earns hands other than mine

## What sayit Refuses to Be

No cloud tier. No account. No telemetry. No subscription, because a key is bought once
with effort and owned forever. No feature that requires explaining sayit to someone
before they can watch it work. The pitch is a demonstration eight seconds long.

## How I'll Know It's Done

Keyboards don't have success metrics; they have reflexes. sayit is done when holding
the key is as unconscious as reaching for backspace — when you catch yourself
mid-sentence in some app you never configured, talking, because your hands decided
speech was faster before your brain weighed in.

The uninstall of WhisperTyping won't be a ceremony. You'll just notice, one day, that
it's been weeks.
