// `npm test`: the pill's pure logic, checked under plain Node. Node ≥ 22.18
// runs TypeScript directly by stripping the types, so there is no test
// framework and no new dependency — each check prints ok or FAIL, and any
// FAIL throws at the end, which exits nonzero (a red test in CI).
// The import says ".ts" because Node, unlike Vite, never guesses extensions.

import { HEARD_NOTHING, REASONS, TIMING, failed, hideAfterMs, lifeMs, reasonFor } from "./pill.ts";
import type { Failure, Reason } from "./pill.ts";

let failures = 0;
const check = (name: string, ok: boolean, detail = "") => {
  if (ok) {
    console.log(`ok   ${name}`);
  } else {
    failures++;
    console.error(`FAIL ${name}${detail ? ` — ${detail}` : ""}`);
  }
};
const expectReason = (name: string, failure: Failure, want: Reason) => {
  const got = reasonFor(failure);
  check(name, got === want, `got ${got}, want ${want}`);
};
const error = (cmd: string, cause: unknown): Failure => ({ kind: "error", cmd, cause });

// ---- the mapping, cause by cause (error texts copied from the Rust) ----

expectReason("press: no device at all", error("start_capture", "no microphone found"), "no-mic");
expectReason(
  "press: hung device",
  error("start_capture", "microphone did not start (device hung after replug?)"),
  "mic-wont-start",
);
expectReason(
  "press: stream refused",
  error("start_capture", "mic stream won't start: The requested device is no longer available."),
  "mic-wont-start",
);
expectReason(
  "release: mic delivered nothing",
  error("stop_and_transcribe", "recording ended before the microphone delivered audio (12/2400 samples)"),
  "mic-no-audio",
);
expectReason(
  "release: engine unreachable for 90s",
  error("stop_and_transcribe", "engine never answered: sidecar unreachable: error sending request"),
  "engine-silent",
);
expectReason(
  "release: engine answered with an error",
  error("stop_and_transcribe", "sidecar returned 500 Internal Server Error: failed to read audio"),
  "engine-error",
);
expectReason(
  "release: an error nobody taught us is an honest shrug",
  error("stop_and_transcribe", "expected value at line 1 column 1"),
  "unknown",
);
expectReason("paste: any inject error", error("inject_text", "Linux input worker did not answer"), "paste-failed");
expectReason(
  "seatbelt: transcribe never came back",
  { kind: "timeout", stage: "transcribing" },
  "engine-silent",
);
// It may have landed before it hung, so the pill can't claim it failed.
expectReason("seatbelt: paste never came back", { kind: "timeout", stage: "injecting" }, "paste-stuck");
expectReason("seatbelt: any other stage", { kind: "timeout", stage: "idle" }, "unknown");
expectReason("an IPC hiccup on a housekeeping command", error("trigger_mode", "ipc failed"), "unknown");
expectReason(
  "a cause that isn't a string still maps",
  error("start_capture", new Error("no microphone found")),
  "no-mic",
);

// ---- the copy: fits the pill, says nothing it shouldn't ----------------

for (const [key, sentence] of Object.entries(REASONS)) {
  // 26 chars is HEARD_NOTHING's length — the longest line the pill was
  // checked with on screen at 280px wide.
  check(`"${key}" fits the pill`, sentence.length <= HEARD_NOTHING.length, `${sentence.length} chars`);
  check(`"${key}" has no codes or paths`, !/[0-9/\\:]/.test(sentence), sentence);
}
const leaky = failed(error("start_capture", "failed to open mic stream: C:\\Users\\k\\x.dll 0x80070005"));
check(
  "a failure verdict never echoes the raw error",
  leaky.state === "failed" && leaky.reason === REASONS["mic-wont-start"],
  JSON.stringify(leaky),
);

// ---- the timing the coordinator and the pill page both read ------------

check("done is a brief resolve", lifeMs({ state: "done" }) <= 400);
check("heard nothing lingers about 2s", lifeMs({ state: "heard-nothing" }) === 2000);
const failedLife = lifeMs({ state: "failed", reason: "x" });
check("a failure lingers 4–6s", failedLife >= 4000 && failedLife <= 6000, `${failedLife}ms`);
check("every verdict outlives its own exit fade", TIMING.doneMs > TIMING.fadeMs);
for (const verdict of [{ state: "done" }, { state: "heard-nothing" }, { state: "failed", reason: "x" }] as const) {
  // The page may hold the thinking light before a verdict; the window
  // must not go before the held verdict has faded out.
  check(
    `the window outlives a held ${verdict.state}`,
    hideAfterMs(verdict) >= TIMING.thinkingMinMs + lifeMs(verdict) + TIMING.settleMs,
  );
}
check("three missed 50ms level ticks before the mic counts as closed", TIMING.micQuietMs >= 150);

if (failures > 0) throw new Error(`${failures} pill check(s) failed`);
