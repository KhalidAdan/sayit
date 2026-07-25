// The coordinator. Rust touches the OS, the sidecar thinks — this file
// makes decisions. It owns the one piece of app-level logic in sayit v1:
// the state machine. In v2 this file is where Effect moves in; the Rust
// side will never notice.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type State = "idle" | "recording" | "transcribing" | "injecting";

let state: State = "idle";
let sidecarReady = false;

function show(next: State) {
  state = next;
  console.log(`[sayit] state: ${next}`);
  document.body.dataset.state = next;
}

listen("sidecar_ready", () => {
  sidecarReady = true;
  console.log("[sayit] sidecar ready — dictation live");
});

listen("pipeline_error", (e) => {
  console.error("[sayit] pipeline error:", e.payload);
});

// The key's voice. Slots are user-supplied files in soundpack/ — a missing
// file is a silent slot, so this is always safe to call.
const sound = (slot: "press" | "refuse" | "accept") =>
  invoke("play_sound", { slot }).catch(() => {});

// Key went down. Start listening — but only from idle: a press that arrives
// while we're still transcribing the previous take is refused, not queued.
// A refused press must be *audible*: the worst dictation experience is
// delivering a sentence to nobody and finding out a second later.
listen("push_started", async () => {
  if (state !== "idle" || !sidecarReady) {
    sound("refuse");
    return;
  }
  try {
    await invoke("start_capture");
    show("recording");
    sound("press");
  } catch (e) {
    console.error("[sayit] could not start capture:", e);
    sound("refuse");
    show("idle");
  }
});

// Key came up. Run the take through the rest of the pipeline.
listen("push_finished", async () => {
  if (state !== "recording") return;
  show("transcribing");
  try {
    const text = await invoke<string>("stop_and_transcribe");
    if (text.trim().length > 0) {
      show("injecting");
      await invoke("inject_text", { text });
      sound("accept");
    }
  } catch (e) {
    console.error("[sayit] pipeline failed:", e);
  } finally {
    show("idle");
  }
});
