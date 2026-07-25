// The coordinator, now in Effect. Rust touches the OS, the sidecar thinks —
// this file makes decisions, and Effect is how the decisions are written.
//
// Reading guide for future-Khalid:
// - An Effect is a *description* of a computation — nothing runs until
//   Effect.runPromise. Building programs as values is the whole trick.
// - Effect.gen + yield* reads like async/await, but errors are typed
//   values in the error channel, not thrown surprises.
// - Ref is Effect's mutable cell; Clock is time-as-a-service (testable,
//   unlike Date.now()).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Clock, Data, Effect, Ref } from "effect";

type State = "idle" | "recording" | "transcribing" | "injecting";

// A failed Tauri command as a typed, tagged error value. catchTag can
// route on the tag; the cause rides along for logging.
class CmdError extends Data.TaggedError("CmdError")<{
  readonly cmd: string;
  readonly cause: unknown;
}> {}

// The bridge from Tauri's promise world into Effect's.
const cmd = <T = void>(name: string, args?: Record<string, unknown>) =>
  Effect.tryPromise({
    try: () => invoke<T>(name, args),
    catch: (cause) => new CmdError({ cmd: name, cause }),
  });

// Sounds, tray text, and the waveform are fire-and-forget: their failure
// must never fail the pipeline, so Effect.ignore erases their errors.
const sound = (slot: "press" | "refuse" | "accept") =>
  cmd("play_sound", { slot }).pipe(Effect.ignore);
const trayStatus = (text: string) =>
  cmd("tray_status", { text }).pipe(Effect.ignore);
const waveform = (visible: boolean) =>
  cmd(visible ? "waveform_show" : "waveform_hide").pipe(Effect.ignore);

const stateRef = Ref.unsafeMake<State>("idle");
const readyRef = Ref.unsafeMake(false);
const lastGapMs = Ref.unsafeMake<number | null>(null);

// One state transition: set the ref, mirror to console/DOM/tray/waveform.
// Every path through the app goes through here — the tray can't lie.
const show = (next: State) =>
  Effect.gen(function* () {
    yield* Ref.set(stateRef, next);
    yield* Effect.sync(() => {
      console.log(`[sayit] state: ${next}`);
      document.body.dataset.state = next;
    });
    const ready = yield* Ref.get(readyRef);
    if (!ready) {
      yield* trayStatus("warming up…");
    } else if (next === "idle") {
      const gap = yield* Ref.get(lastGapMs);
      yield* trayStatus(
        gap === null ? "ready — hold F9 to dictate" : `ready — last take ${gap}ms`,
      );
    } else {
      yield* trayStatus(
        { recording: "listening…", transcribing: "thinking…", injecting: "typing…" }[next],
      );
    }
    yield* waveform(next === "recording");
  });

// Key went down. Record only from a ready idle; anything else is refused,
// audibly — the worst dictation experience is talking to nobody.
const onPushStarted = Effect.gen(function* () {
  const state = yield* Ref.get(stateRef);
  const ready = yield* Ref.get(readyRef);
  if (state !== "idle" || !ready) {
    return yield* sound("refuse");
  }
  yield* cmd("start_capture");
  yield* show("recording");
  yield* sound("press");
}).pipe(
  Effect.catchTag("CmdError", (e) =>
    Effect.gen(function* () {
      yield* Effect.sync(() => console.error(`[sayit] ${e.cmd} failed:`, e.cause));
      yield* sound("refuse");
      yield* show("idle");
    }),
  ),
);

// Key came up. Run the take through the pipeline, timing the gap — the
// number the whole project is judged by. ensuring() is Effect's `finally`:
// whatever happens, we return to idle.
const onPushFinished = Effect.gen(function* () {
  if ((yield* Ref.get(stateRef)) !== "recording") return;
  const t0 = yield* Clock.currentTimeMillis;
  yield* show("transcribing");
  const text = yield* cmd<string>("stop_and_transcribe");
  if (text.trim().length > 0) {
    yield* show("injecting");
    yield* cmd("inject_text", { text });
    yield* sound("accept");
    const gap = Number((yield* Clock.currentTimeMillis) - t0);
    yield* Ref.set(lastGapMs, gap);
    yield* cmd("log_gap", { totalMs: gap, chars: text.length }).pipe(Effect.ignore);
  }
}).pipe(
  Effect.catchTag("CmdError", (e) =>
    Effect.sync(() => console.error(`[sayit] ${e.cmd} failed:`, e.cause)),
  ),
  Effect.ensuring(show("idle")),
);

const becomeReady = Effect.gen(function* () {
  const already = yield* Ref.get(readyRef);
  if (already) return; // event and startup poll can both land; first wins
  yield* Effect.sync(() => console.log("[sayit] sidecar ready — dictation live"));
  yield* Ref.set(readyRef, true);
  yield* show("idle");
});

// Race-proofing: the sidecar may warm up before this page's listeners
// exist (cached CUDA kernels make warmup fast after the first-ever run),
// so we PULL readiness once at startup and also listen for the push.
void Effect.runPromise(
  Effect.gen(function* () {
    if (yield* cmd<boolean>("is_ready")) yield* becomeReady;
  }).pipe(Effect.ignore),
);

// The edges of the world: Tauri events arrive as callbacks, and each one
// launches its Effect program.
listen("push_started", () => void Effect.runPromise(onPushStarted));
listen("push_finished", () => void Effect.runPromise(onPushFinished));
listen("sidecar_ready", () => void Effect.runPromise(becomeReady));
listen("pipeline_error", (e) =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Effect.sync(() => console.error("[sayit] pipeline error:", e.payload));
      yield* trayStatus("error — check logs");
    }),
  ),
);
