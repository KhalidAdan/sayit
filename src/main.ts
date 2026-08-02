// The coordinator, in Effect. Rust touches the OS, the sidecar thinks —
// this file makes decisions, including the engine's metabolism: awake
// while you dictate, asleep after ten idle minutes, woken by the key
// itself. The user never manages any of it. The key always works.
//
// Reading guide for future-Khalid:
// - An Effect is a *description* — nothing runs until Effect.runPromise.
// - Effect.gen + yield* reads like async/await with typed errors.
// - Ref is a mutable cell; Clock is time-as-a-service; a Fiber is a
//   running Effect you can interrupt — our sleep timer is one.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Clock, Data, Duration, Effect, Fiber, Ref } from "effect";

type State = "idle" | "recording" | "transcribing" | "injecting";
type Engine = "sleeping" | "waking" | "ready";
type TriggerMode = "idle" | "starting" | "recording" | "busy";
type PlatformInfo = { os: "windows" | "linux"; triggerHint: string };

// What stop_and_transcribe returns now: the text plus where the Rust side
// spent its time. Mirrors `Take` in lib.rs (serde renames to camelCase).
type Take = {
  text: string;
  audioMs: number;
  stopMs: number;
  engineWaitMs: number;
  attempts: number;
  wavMs: number;
  httpMs: number;
  parseMs: number;
  dictUs: number;
};

// Mirrors `InjectTiming` in inject.rs. visibleMs is the moment the text is
// on screen; everything after is clipboard-restore politeness.
type InjectTiming = {
  clipboardSaveMs: number;
  clipboardSetMs: number;
  settleMs: number;
  keystrokeMs: number;
  visibleMs: number;
  restoreWaitMs: number;
  totalMs: number;
};

/// After this much idle, the engine sleeps and ~500MB of VRAM comes home.
const IDLE_SLEEP = Duration.minutes(10);

class CmdError extends Data.TaggedError("CmdError")<{
  readonly cmd: string;
  readonly cause: unknown;
}> {}

const cmd = <T = void>(name: string, args?: Record<string, unknown>) =>
  Effect.tryPromise({
    try: () => invoke<T>(name, args),
    catch: (cause) => new CmdError({ cmd: name, cause }),
  });

// Fire-and-forget concerns: their failure must never fail the pipeline.
const sound = (slot: "press" | "refuse" | "accept") =>
  cmd("play_sound", { slot }).pipe(Effect.ignore);
const trayStatus = (text: string) =>
  cmd("tray_status", { text }).pipe(Effect.ignore);
const waveform = (visible: boolean) =>
  cmd(visible ? "waveform_show" : "waveform_hide").pipe(Effect.ignore);

const stateRef = Ref.unsafeMake<State>("idle");
// The quick-tap seam: a sub-100ms tap delivers push_finished while
// onPushStarted is still awaiting start_capture, so stateRef says "idle"
// and the release used to be dropped — leaving the mic recording forever.
// pressInFlight marks that window; releasedDuringStart parks the key-up
// stamp for the press fiber's finalizer to replay.
const pressInFlightRef = Ref.unsafeMake(false);
const releasedDuringStartRef = Ref.unsafeMake<number | null>(null);
const heardNothingRef = Ref.unsafeMake(false);
const engineRef = Ref.unsafeMake<Engine>("waking"); // boot starts the engine
const keepAwakeRef = Ref.unsafeMake(false);
const lastGapMs = Ref.unsafeMake<number | null>(null);
const updateReadyRef = Ref.unsafeMake<string | null>(null);
const platformRef = Ref.unsafeMake<PlatformInfo>({
  os: "windows",
  triggerHint: "hold F9 to dictate",
});
const sleepTimerRef = Ref.unsafeMake<Fiber.RuntimeFiber<void, never> | null>(null);

// ---- the engine's metabolism ----------------------------------------

const cancelSleepTimer = Effect.gen(function* () {
  const fiber = yield* Ref.get(sleepTimerRef);
  // interruptFork, not interrupt: never WAIT for the timer to die — a
  // press once sat behind a 10-minute uninterruptible sleep this way.
  if (fiber) yield* Fiber.interruptFork(fiber);
  yield* Ref.set(sleepTimerRef, null);
});

// Armed on every return to idle; interrupted by the next press. If it
// ever fires, the engine sleeps — invisibly, announced only by the tray.
const armSleepTimer = Effect.gen(function* () {
  yield* cancelSleepTimer;
  const keepAwake = yield* Ref.get(keepAwakeRef);
  const engine = yield* Ref.get(engineRef);
  if (keepAwake || engine !== "ready") return;
  // Effect.interruptible is load-bearing: this fork happens inside
  // show("idle"), which runs inside ensuring() — a finalizer, which is
  // UNINTERRUPTIBLE, and forked fibers inherit that. Without this marker
  // the sleep cannot be interrupted and cancellation waits out the full
  // ten minutes (regression ledger: the silent second-press bug).
  const fiber = yield* Effect.sleep(IDLE_SLEEP).pipe(
    Effect.zipRight(cmd("engine_sleep").pipe(Effect.ignore)),
    Effect.interruptible,
    Effect.forkDaemon,
  );
  yield* Ref.set(sleepTimerRef, fiber);
});

// ---- one state transition, mirrored everywhere ----------------------

// Explicit return type: show() re-invokes itself (the heard-nothing tray
// revert), and TypeScript needs the annotation to allow the recursion.
const show = (next: State): Effect.Effect<void> =>
  Effect.gen(function* () {
    yield* Ref.set(stateRef, next);
    yield* Effect.sync(() => {
      console.log(`[sayit] state: ${next}`);
      document.body.dataset.state = next;
    });
    if (next === "idle") {
      yield* cmd("trigger_mode", { mode: "idle" satisfies TriggerMode }).pipe(Effect.ignore);
      const engine = yield* Ref.get(engineRef);
      const heardNothing = yield* Ref.get(heardNothingRef);
      if (heardNothing) {
        // A take transcribed to empty: say so instead of pretending
        // nothing happened, then revert to the normal idle text.
        yield* Ref.set(heardNothingRef, false);
        yield* trayStatus("heard nothing — mic muted or dead?");
        yield* Effect.sleep(Duration.seconds(4)).pipe(
          Effect.zipRight(
            Effect.gen(function* () {
              if ((yield* Ref.get(stateRef)) === "idle") yield* show("idle");
            }),
          ),
          Effect.forkDaemon,
        );
      } else if (engine === "sleeping") {
        yield* trayStatus("engine sleeping — press to talk");
      } else if (engine === "waking") {
        yield* trayStatus("waking up…");
      } else {
        const gap = yield* Ref.get(lastGapMs);
        const update = yield* Ref.get(updateReadyRef);
        const platform = yield* Ref.get(platformRef);
        const base =
          gap === null ? `ready — ${platform.triggerHint}` : `ready — last take ${gap}ms`;
        yield* trayStatus(
          update === null ? base : `${base} · v${update} ready on restart`,
        );
      }
      yield* armSleepTimer;
    } else {
      yield* trayStatus(
        { recording: "listening…", transcribing: "thinking…", injecting: "typing…" }[next],
      );
    }
    yield* waveform(next === "recording");
  });

// ---- the pipeline ---------------------------------------------------

// Key went down. From idle the press ALWAYS records — engine awake or
// not. Capture needs no engine; only transcription does, and it knows
// how to wait. Refusal is reserved for mid-pipeline presses.
const onPushStarted = Effect.gen(function* () {
  if ((yield* Ref.get(stateRef)) !== "idle") {
    return yield* sound("refuse");
  }
  yield* Ref.set(pressInFlightRef, true);
  yield* Ref.set(releasedDuringStartRef, null);
  yield* cancelSleepTimer;
  if ((yield* Ref.get(engineRef)) === "sleeping") {
    yield* cmd("engine_start").pipe(Effect.ignore); // idempotent wake
    yield* Ref.set(engineRef, "waking");
  }
  yield* cmd("start_capture");
  yield* cmd("trigger_mode", { mode: "recording" satisfies TriggerMode });
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
  // Replay a release that outran start_capture. If the press failed, state
  // is back to idle and onPushFinished discards the parked stamp. Forked,
  // not inlined: this is a finalizer, finalizers are UNINTERRUPTIBLE, and
  // the take's 110s seatbelt works by interruption (see armSleepTimer).
  Effect.ensuring(
    Effect.gen(function* () {
      yield* Ref.set(pressInFlightRef, false);
      const parked = yield* Ref.get(releasedDuringStartRef);
      yield* Ref.set(releasedDuringStartRef, null);
      if (parked !== null) {
        yield* onPushFinished(parked).pipe(Effect.interruptible, Effect.forkDaemon);
      }
    }),
  ),
);

// Key came up. stop_and_transcribe is patient on the Rust side: if this
// take raced an engine wake, it waits for warmth instead of failing.
// keyUpMs is the OS-event wall-clock stamp from hotkey.rs — the take's
// true t0, from before the event even reached this webview.
const onPushFinished = (keyUpMs: number): Effect.Effect<void> => Effect.gen(function* () {
  if ((yield* Ref.get(stateRef)) !== "recording") {
    // The release beat start_capture (a sub-100ms tap). Park the stamp
    // for the press fiber's finalizer — dropping it left the mic
    // recording until the next tap (regression ledger: the quick-tap
    // stuck-recording bug). Any other stray release is ignored, and
    // deliberately does NOT pass through the take's show("idle").
    if (yield* Ref.get(pressInFlightRef)) {
      yield* Ref.set(releasedDuringStartRef, keyUpMs);
    }
    return;
  }
  yield* runTake(keyUpMs);
});

const runTake = (keyUpMs: number) => Effect.gen(function* () {
  const tEntry = yield* Clock.currentTimeMillis;
  const t0 = keyUpMs > 0 ? keyUpMs : tEntry;
  yield* cmd("trigger_mode", { mode: "busy" satisfies TriggerMode }).pipe(Effect.ignore);
  yield* show("transcribing");
  // Timeout is the coordinator's own seatbelt: whatever the Rust side
  // does — hung device, dead engine — this state machine returns to idle.
  // 100s clears transcribe_waiting's 90s patience with room to spare.
  const tCall = yield* Clock.currentTimeMillis;
  const take = yield* cmd<Take>("stop_and_transcribe").pipe(
    Effect.timeout(Duration.seconds(100)),
  );
  const tBack = yield* Clock.currentTimeMillis;
  if (take.text.trim().length > 0) {
    yield* show("injecting");
    const tInject = yield* Clock.currentTimeMillis;
    const inject = yield* cmd<InjectTiming>("inject_text", { text: take.text });
    const tDone = yield* Clock.currentTimeMillis;
    yield* sound("accept");

    // Three clocks meet here: the OS stamp, this webview, the Rust stages.
    const totalMs = tDone - t0; // what the old log called "the gap"
    // The user stopped waiting when the text appeared — before inject's
    // clipboard-restore wait. THIS is the gap the north star talks about.
    const feltMs = totalMs - (inject.totalMs - inject.visibleMs);
    const dispatchMs = tEntry - t0;
    // Coordinator housekeeping: tray + waveform IPC around the commands.
    const preMs = tCall - tEntry + (tInject - tBack);
    const rustAccounted =
      take.stopMs + take.engineWaitMs + take.wavMs + take.httpMs + take.parseMs;
    // What the invoke round-trips cost beyond the work they carried.
    const ipcMs =
      Math.max(0, tBack - tCall - rustAccounted) +
      Math.max(0, tDone - tInject - inject.totalMs);

    const pct = (ms: number) => `${Math.round((ms / feltMs) * 100)}%`.padStart(4);
    const row = (label: string, ms: number, note = "") =>
      `[gap]   ${label.padEnd(13)}${String(ms).padStart(6)}ms ${pct(ms)}  ${note}`;
    console.log(
      [
        `[gap] ━━ felt ${feltMs}ms · total ${totalMs}ms · ${(take.audioMs / 1000).toFixed(1)}s audio · ${take.text.length} chars`,
        row("dispatch", dispatchMs, "key-up event → coordinator"),
        row("housekeeping", preMs, "tray + waveform IPC"),
        row("mic stop", take.stopMs, "teardown + resample"),
        row("engine wait", take.engineWaitMs, `${take.attempts} attempt${take.attempts === 1 ? "" : "s"}`),
        row("wav encode", take.wavMs),
        row("inference", take.httpMs, "whisper, http round-trip"),
        row("parse", take.parseMs),
        row("dictionary", Math.round(take.dictUs / 1000), `(${take.dictUs}µs)`),
        row("ipc toll", ipcMs, "webview ↔ rust, both commands"),
        row("inject", inject.visibleMs, "clipboard + settle + ctrl-v → VISIBLE"),
        `[gap]   (+${inject.totalMs - inject.visibleMs}ms clipboard restore after the text landed — excluded from felt)`,
      ].join("\n"),
    );

    yield* Ref.set(lastGapMs, feltMs);
    yield* cmd("log_gap", {
      row: {
        totalMs,
        feltMs,
        chars: take.text.length,
        audioMs: take.audioMs,
        dispatchMs,
        preMs,
        stopMs: take.stopMs,
        engineWaitMs: take.engineWaitMs,
        wavMs: take.wavMs,
        httpMs: take.httpMs,
        parseMs: take.parseMs,
        dictUs: take.dictUs,
        ipcMs,
        injectVisibleMs: inject.visibleMs,
        injectTotalMs: inject.totalMs,
      },
    }).pipe(Effect.ignore);
  } else {
    // An empty take must be FELT: woosh now, tray explains via the
    // heardNothing flag when ensuring() returns us to idle.
    yield* sound("refuse");
    yield* Ref.set(heardNothingRef, true);
  }
}).pipe(
  // The WHOLE take wears the seatbelt — transcribe, inject, everything.
  // (Learned the hard way: a hung clipboard after a successful paste
  // wedged the flow *after* the text had visibly landed.)
  Effect.timeout(Duration.seconds(110)),
  // CmdError = a stage failed; TimeoutException = a stage never answered.
  // Both end the same way: audible refusal, back to idle, key usable.
  Effect.catchAll((e) =>
    Effect.gen(function* () {
      yield* Effect.sync(() => console.error("[sayit] take failed:", e));
      yield* sound("refuse");
    }),
  ),
  Effect.ensuring(show("idle")),
);

// ---- engine + readiness events --------------------------------------

const becomeReady = Effect.gen(function* () {
  if ((yield* Ref.get(engineRef)) === "ready") return;
  yield* Effect.sync(() => console.log("[sayit] engine ready — dictation live"));
  yield* Ref.set(engineRef, "ready");
  if ((yield* Ref.get(stateRef)) === "idle") yield* show("idle");
});

// Race-proofing: warmup may finish before this page's listeners exist,
// so we PULL readiness once at startup and also listen for the push.
// The persisted keep-awake preference is pulled the same way.
void Effect.runPromise(
  Effect.gen(function* () {
    yield* Ref.set(platformRef, yield* cmd<PlatformInfo>("platform_info"));
    yield* Ref.set(keepAwakeRef, yield* cmd<boolean>("get_keep_awake"));
    if (yield* cmd<boolean>("is_ready")) yield* becomeReady;
  }).pipe(Effect.ignore),
);

// Payloads are wall-clock stamps from hotkey.rs, taken at the OS event.
listen<number>("push_started", (e) => {
  if (e.payload > 0) console.log(`[gap] press dispatch: ${Date.now() - e.payload}ms`);
  void Effect.runPromise(onPushStarted);
});
listen<number>("push_finished", (e) => void Effect.runPromise(onPushFinished(e.payload)));
listen("sidecar_ready", () => void Effect.runPromise(becomeReady));

listen("engine_waking", () =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Ref.set(engineRef, "waking");
      if ((yield* Ref.get(stateRef)) === "idle") yield* show("idle");
    }),
  ),
);

listen("engine_sleeping", () =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Ref.set(engineRef, "sleeping");
      if ((yield* Ref.get(stateRef)) === "idle") yield* show("idle");
    }),
  ),
);

listen<boolean>("keep_awake", (e) =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Ref.set(keepAwakeRef, e.payload);
      if (e.payload) {
        yield* cancelSleepTimer;
        // Pinning awake while asleep also wakes it: the user asked for a
        // hot engine, give them one.
        if ((yield* Ref.get(engineRef)) === "sleeping") {
          yield* cmd("engine_start").pipe(Effect.ignore);
        }
      } else if ((yield* Ref.get(stateRef)) === "idle") {
        yield* armSleepTimer;
      }
    }),
  ),
);

// An update was downloaded and staged (update.rs); it applies on next
// launch. Never interrupt the user — just note it in the idle tray text.
listen<string>("update_installed", (e) =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Ref.set(updateReadyRef, e.payload);
      if ((yield* Ref.get(stateRef)) === "idle") yield* show("idle");
    }),
  ),
);

listen("pipeline_error", (e) =>
  void Effect.runPromise(
    Effect.gen(function* () {
      yield* Effect.sync(() => console.error("[sayit] pipeline error:", e.payload));
      yield* trayStatus("error — check logs");
    }),
  ),
);
