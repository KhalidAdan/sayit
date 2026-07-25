# Architecture — how sayit sits in the machine

One diagram, kept honest: if the code and this drawing disagree, one of
them is a bug. See the north star for why the shape is this way.

```mermaid
flowchart TD
    subgraph HW["Hardware"]
        KB[Keyboard]
        MIC[Microphone]
        GPU["RTX 2070 (CUDA)"]
        SPK[Speakers]
    end

    subgraph OS["Windows"]
        HOTAPI["Global hotkey API"]
        WASAPI["Audio (WASAPI)"]
        CLIP[Clipboard]
        FOCUS["Whatever app has focus"]
        TRAYAREA[System tray]
    end

    subgraph APP["sayit process (Tauri)"]
        subgraph RUST["Rust host — touches the OS"]
            HOT[hotkey.rs]
            CAP[capture.rs]
            TRA[transcribe.rs]
            INJ[inject.rs]
            SND[sounds.rs]
            TRY[tray.rs]
            SIDE[sidecar.rs]
        end
        subgraph WEB["WebView2 — makes decisions"]
            COORD["main.ts (Effect coordinator)"]
            WAVE["waveform.ts (overlay window)"]
        end
    end

    subgraph EXT["Sidecar process"]
        WS["whisper-server.exe"]
    end

    subgraph DISK["Disk"]
        MODEL["models/ggml-small.bin"]
        PACK["soundpack/*.ogg|wav"]
        GAPLOG["gap-log.csv"]
    end

    KB --> HOTAPI --> HOT
    HOT -- "push_started / push_finished" --> COORD
    COORD -- "start_capture / stop_and_transcribe / inject_text" --> RUST
    MIC --> WASAPI --> CAP
    CAP -- "mic_level every 50ms" --> WAVE
    CAP -- "16kHz mono f32" --> TRA
    TRA -- "WAV over localhost HTTP" --> WS
    WS --> GPU
    MODEL --> WS
    SIDE -- "spawn / warmup / kill" --> WS
    INJ -- "set text + Ctrl+V" --> CLIP --> FOCUS
    PACK --> SND --> SPK
    TRY --> TRAYAREA
    COORD -- "tray_status" --> TRY
    COORD -- "log_gap" --> GAPLOG
```

Three sentences that summarize the whole design:

1. **Rust touches the OS, TS makes decisions, the sidecar thinks** — every
   arrow between the webview and Rust is either an event up or a command
   down; that seam is the whole contract.
2. The four pipeline stages (hotkey → capture → transcribe → inject) never
   talk to each other — audio and text flow forward through the
   coordinator, and each stage can be replaced without the others noticing.
3. Everything stays on this machine: the model is a child process on
   localhost, the audio dies after transcription, and the only artifacts
   are the text at your cursor and a row in gap-log.csv.
```
