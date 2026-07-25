# Where browser-local STT gets interesting

Riff from 2026-07-22. The browser sandbox kills system-wide dictation, but stops
mattering the moment *the web app itself is the destination*. So: which apps get better
when they can hear you — free, offline, audio never leaving the device? That last clause
is the sleeper feature.

## App ideas

- **The privacy-locked transcriber.** Therapist session notes, legal dictation,
  journalists with vulnerable sources. Client-side inference means there is no server to
  breach or subpoena — provable with the network tab open. "Your audio never leaves this
  tab" is a feature no cloud competitor can copy, structurally. Most underrated idea here.
- **Edit video by editing text** (Descript's trick, free in a browser). Whisper's
  word-level timestamps make the transcript a scrubber for time: delete a sentence,
  cut the video. Searchable/clickable podcast players; every recorded meeting becomes
  a document.
- **Language learning with real ears.** App knows what you were *supposed* to say;
  Whisper reports what you *did* say; the diff is pronunciation feedback. Private,
  unlimited, and nobody hears you butcher French vowels — embarrassment is the real
  barrier in speaking practice.
- **Voice as a textarea upgrade, everywhere.** Every SaaS app with a big text field
  (CRM call logs, bug trackers, EHRs) embeds a locally-running mic button. No per-seat
  API costs, no audio-compliance questions. Won't make headlines; will quietly appear
  everywhere, like spellcheck did.
- **Voice as the input layer for LLM apps** — the era-defining one. Talking is 3–4x
  faster than typing, and LLMs are the first software that absorbs unstructured rambling
  and returns structure. STT gives you words; STT → LLM gives you *intent*. Mumble
  "meeting with Sarah went well, she wants the proposal Friday, loop in accounting" and
  the task manager files three structured items. Seventy years of keyboards forced humans
  to think in the computer's format; this stack inverts it.

## The thesis underneath

**Inference is moving to the edge and its marginal cost is going to zero.** Cloud AI
makes every user action cost the developer money (hence subscriptions and usage caps).
Client-side models let a free static page ship intelligence the way it ships fonts. When
a capability's marginal cost hits zero it stops being a product and becomes an
*ingredient* — nobody sells "spellcheck" anymore.

## Prediction

Browsers ship local STT as a built-in API within a few years. `SpeechRecognition`
already exists (Chrome historically piped it to Google servers; on-device experiments
underway). Expect a `navigator`-level "transcribe this stream locally" call, the way
geolocation is just *there* — at which point every idea above becomes a weekend project
for a web dev.
