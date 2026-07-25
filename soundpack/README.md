# soundpack — the key's voice

Drop audio files here and sayit plays them. No config, no settings screen:
the filename is the hook.

| file | plays when |
|---|---|
| `press.wav` / `press.ogg` | recording started — the key heard you |
| `refuse.wav` / `refuse.ogg` | press ignored (sidecar still warming, or previous take still processing) — you are talking to nobody, stop |
| `accept.wav` / `accept.ogg` | text landed in your app |

Rules:

- WAV or OGG. If both exist for a slot, WAV wins.
- A missing file is a silent slot. Silence is a legitimate choice — `accept`
  is deliberately empty, because the text appearing *is* the confirmation.
- Keep them short (under ~300ms) and quiet. `press` plays while the mic is
  already recording; a long or loud sound can bleed into your take.
- Files load once at app start — after swapping sounds, restart sayit.

Current pack: `refuse.ogg` is "woosh 01" from the GRAND ADVENTURE asset pack
(Joel Steudler). `press` is vacant pending a sufficiently creamy thock.

License note (checked 2026-07-25): `refuse.ogg` is from a licensed pack
whose grant covers use "in your games" (Joel Steudler, Grand Adventure).
Khalid owns the pack and keeps the raw assets elsewhere; committing this
one file to the private repo is a deliberate call. Revisit before the repo
goes public or sayit ships to strangers: get Joel Steudler's OK or swap in
a CC0 sound (e.g. Kenney's Interface Sounds pack).
