# sayit

**The key that listens.** Fast, local dictation for Windows and Linux.

- **Linux:** double-tap **left Control** (or Caps Lock when GNOME maps it to Control) to start; tap it once to finish.
- **Windows:** hold **F9** to dictate; release it to finish.
- Audio is transcribed locally by Whisper and pasted into the focused app.
- No account, cloud API, telemetry, or per-use cost.

## Linux

The first supported Linux target is GNOME on Wayland with PipeWire, an NVIDIA
RTX 3070, and x86_64. Ghostty, Helium, and Zed are the acceptance applications.

A first-run window checks the microphone, readable keyboard event device, and
`/dev/uinput`; downloads and verifies the model/engine if needed; warms the
engine; and contains a test paste. Later failures are reported in the tray under
**Diagnostics…**.

sayit does **not** grab the keyboard. It examines input events only to recognize
clean left-Control/remapped-Caps taps, discards every other event immediately, and never logs
or persists typed keys. Injection saves both Linux clipboard selections,
pastes through a process-owned virtual keyboard, and restores the selections
unless the user changed them in the meantime.

### Install a built or downloaded binary

```bash
scripts/install-linux.sh [path/to/sayit-linux-x86_64]
```

This is a rootless per-user install at `~/.local/bin/sayit`. Runtime assets live
under `${XDG_DATA_HOME:-~/.local/share}/dev.khalid.sayit`. The script does not
install packages. Released builds bootstrap a signed CUDA companion when
available and fall back to the pinned official CPU engine.

Host runtime libraries are GTK 3, WebKitGTK 4.1, ALSA/PipeWire, and an active
StatusNotifierItem tray extension. NVIDIA users need only the normal driver;
the CUDA runtime is part of sayit's companion.

## Development

```bash
npm ci
npm run build
cd src-tauri
cargo test
cargo run
```

On immutable Linux hosts, build inside a Fedora/Ubuntu Distrobox containing the
GTK/WebKitGTK/ALSA development headers. CUDA compilation is isolated in the
NVIDIA build container described by `.github/workflows/engine-linux.yml`; no
CUDA toolkit is installed on the desktop host.

See:

- [`docs/building-sayit.md`](docs/building-sayit.md) — architecture and build notes
- [`docs/sidecar.md`](docs/sidecar.md) — local inference companion and measured latency
- [`docs/test-plan.md`](docs/test-plan.md) — trust and release gates
