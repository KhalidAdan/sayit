// The pill's contract: what the waveform window can say, for how long, and
// in which words. Two webviews import this file — main.ts decides WHEN the
// pill changes, waveform.ts decides how each state LOOKS — so it is the
// one place they agree, and the one place the copy lives.
//
// Pure on purpose: no Tauri, no Effect, no DOM. That's what lets
// `npm test` run it under plain Node (see pill.test.ts).

/// Every word the coordinator can send the pill. These say what the
/// PIPELINE is doing; waveform.ts turns them into what the pill SHOWS, and
/// the two can differ for a moment, because the mic is the truth:
/// - "thinking" — the key is up and the take is in stop_and_transcribe.
///   The mic may still be open (a release tail keeps it listening past
///   key-up), so the pill keeps the recording bars for as long as mic
///   levels keep arriving, and shows the thinking light once they stop.
/// - "typing" — stop_and_transcribe returned, so the mic is certainly
///   closed: the thinking light, whatever the levels did. (The look is
///   the same for transcribing and injecting: to the user it's one wait.)
export type Pill =
  | { readonly state: "hidden" }
  | { readonly state: "recording" }
  | { readonly state: "thinking" }
  | { readonly state: "typing" }
  | Verdict;

/// How a take ended — the pill's last word before it hides.
export type Verdict =
  | { readonly state: "done" }
  | { readonly state: "heard-nothing" }
  | { readonly state: "failed"; readonly reason: string };

export const TIMING = {
  /// Capture emits a mic level every 50ms, silence included, for exactly
  /// as long as the mic is open. Three missed ticks means it closed.
  micQuietMs: 150,
  /// The thinking light stays up at least this long before a verdict
  /// replaces it, so a fast take still registers as "it worked on that"
  /// instead of a flicker. Held by the pill page, which alone knows when
  /// the light actually came on.
  thinkingMinMs: 200,
  /// Each verdict's visible life. Its exit fade ends exactly here.
  doneMs: 300,
  heardNothingMs: 2000,
  failedMs: 5000,
  /// Every verdict fades out over the last fadeMs of its life.
  fadeMs: 160,
  /// After a verdict's life, the coordinator waits this much longer before
  /// hiding the window, so the faded-out (empty) frame is the one on screen
  /// when it goes — and the one a later show briefly re-shows.
  settleMs: 60,
} as const;

export const lifeMs = (v: Verdict): number =>
  v.state === "done"
    ? TIMING.doneMs
    : v.state === "heard-nothing"
      ? TIMING.heardNothingMs
      : TIMING.failedMs;

/// When the coordinator hides the window, counted from the moment it sends
/// the verdict: the page may first hold the thinking light for up to
/// thinkingMinMs, then the verdict lives its life, then the empty frame
/// settles. (When nothing was held, the plate is simply invisible — and
/// click-through — for that last stretch.)
export const hideAfterMs = (v: Verdict): number =>
  TIMING.thinkingMinMs + lifeMs(v) + TIMING.settleMs;

export const HEARD_NOTHING = "heard nothing — mic muted?";

// ---- why a take failed, in plain words --------------------------------

/// What the coordinator knows when a take fails: a command that answered
/// with an error (plus the text Rust sent with it), or a stage that never
/// answered at all. Effect's TimeoutException carries no command name, but
/// the state machine knows what it was waiting on.
export type Failure =
  | { readonly kind: "error"; readonly cmd: string; readonly cause: unknown }
  | { readonly kind: "timeout"; readonly stage: string };

/// One short sentence per cause. Lowercase and unpunctuated like every
/// other tray line, because each one also appears there as
/// "ready — last take failed: <reason>". No codes, no paths — the raw
/// error stays in the console log, next to the reason it was mapped to.
export const REASONS = {
  "no-mic": "no mic found",
  "mic-wont-start": "the mic won't start",
  "mic-no-audio": "the mic sent no audio",
  "engine-silent": "the engine didn't answer",
  "engine-error": "the engine hit an error",
  "paste-failed": "couldn't paste the text",
  // A paste that never returned may or may not have landed first.
  "paste-stuck": "the paste got stuck",
  unknown: "something went wrong",
} as const;

export type Reason = keyof typeof REASONS;

/// Which sentence a failure deserves. Matching on Rust's error text is
/// positive identification only: a message this function doesn't
/// recognise becomes "unknown" — an honest shrug — never a confident
/// wrong reason. The texts matched live in capture.rs (start, stop) and
/// transcribe.rs (transcribe_waiting, transcribe).
export const reasonFor = (failure: Failure): Reason => {
  if (failure.kind === "timeout") {
    // The take's seatbelts (main.ts runTake): the stage that was running
    // when time ran out is the stage that hung.
    if (failure.stage === "transcribing") return "engine-silent";
    if (failure.stage === "injecting") return "paste-stuck";
    return "unknown";
  }
  const text = String(failure.cause);
  switch (failure.cmd) {
    case "start_capture":
      // Everything start_capture can say is the mic refusing to start;
      // only a missing device gets its own, more useful, sentence.
      return text.includes("no microphone found") ? "no-mic" : "mic-wont-start";
    case "stop_and_transcribe":
      if (text.includes("before the microphone delivered audio")) return "mic-no-audio";
      if (text.startsWith("engine never answered")) return "engine-silent";
      if (text.startsWith("sidecar returned")) return "engine-error";
      return "unknown";
    case "inject_text":
      // inject_text does exactly one thing: the paste.
      return "paste-failed";
    default:
      return "unknown";
  }
};

/// The pill's failure verdict for a failure.
export const failed = (failure: Failure): Extract<Verdict, { state: "failed" }> => ({
  state: "failed",
  reason: REASONS[reasonFor(failure)],
});
