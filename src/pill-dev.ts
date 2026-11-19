// Dev only: a state switcher for working on the pill in a plain browser.
// `npm run dev`, then open /waveform.html. waveform.ts loads this file
// only when Vite is serving AND there is no Tauri backend, from a branch a
// production build deletes — so it never ships, and never appears in the
// real pill during `tauri dev` either.
//
// It plays the coordinator's part (each verdict hides after hideAfterMs,
// exactly like main.ts) and capture's (a made-up voice sends a level every
// 50ms while the "mic" is open). URL parameters pin one moment, for
// screenshots: ?pill=failed&reason=engine-silent&at=900&panel=0
//   pill   recording | thinking | done | heard-nothing | failed
//   reason a key of REASONS (failed only)
//   at     freeze the look this many ms after it began
//   panel  0 hides the buttons

import { REASONS, TIMING, hideAfterMs, type Pill, type Reason, type Verdict } from "./pill";

type Hooks = {
  apply: (pill: Pill) => void;
  pushLevel: (peak: number) => void;
  freeze: (at: number | null) => void;
};

/// A made-up voice: syllables riding slower phrases, as raw mic peaks.
/// Deterministic, so a frozen screenshot is the same every run.
const voice = (k: number) =>
  0.003 + 0.25 * Math.abs(Math.sin(k * 0.9)) ** 1.5 * (0.5 + 0.5 * Math.sin(k * 0.21));

const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));

export function mount({ apply, pushLevel, freeze }: Hooks) {
  // The window is 280×64 in the app; here, a stage around the same pill.
  const style = document.createElement("style");
  style.textContent = `
    html, body { background: linear-gradient(160deg, #52525b, #a1a1aa) !important; }
    #pill { inset: calc(50% - 32px) auto auto calc(50% - 140px) !important;
            width: 280px; height: 64px; }
    #dev { position: fixed; top: 8px; left: 8px; display: flex; flex-wrap: wrap; gap: 4px;
           max-width: calc(100% - 16px); font: 12px system-ui, sans-serif; }
    #dev button, #dev select { font: inherit; padding: 2px 6px; }`;
  document.head.append(style);

  let tick = 0;
  let mic: number | undefined;
  let timers: number[] = [];
  const micOpen = (on: boolean) => {
    clearInterval(mic);
    if (on) mic = window.setInterval(() => pushLevel(voice(tick++)), 50);
  };
  const later = (ms: number, run: () => void) => timers.push(window.setTimeout(run, ms));
  const reset = () => {
    timers.forEach(clearTimeout);
    timers = [];
    freeze(null);
  };
  // A verdict's whole life, as main.ts's settlePill runs it.
  const settle = (verdict: Verdict) => {
    micOpen(false);
    apply(verdict);
    later(hideAfterMs(verdict), () => apply({ state: "hidden" }));
  };
  const failure = (reason: Reason): Verdict => ({ state: "failed", reason: REASONS[reason] });

  // A take as the coordinator and capture would play it. tailMs keeps the
  // mic open past key-up, the way a release tail does.
  const take = (speakMs: number, tailMs: number, transcribeMs: number) => {
    reset();
    micOpen(true);
    apply({ state: "recording" });
    later(speakMs, () => apply({ state: "thinking" })); // key-up
    later(speakMs + tailMs, () => micOpen(false)); // the mic closes
    const back = speakMs + tailMs + transcribeMs; // stop_and_transcribe returns
    later(back, () => apply({ state: "typing" }));
    later(back + 370, () => settle({ state: "done" })); // inject_text returns
  };

  const params = new URLSearchParams(location.search);
  if (params.get("panel") !== "0") {
    const panel = document.createElement("div");
    panel.id = "dev";
    const reasons = document.createElement("select");
    for (const key of Object.keys(REASONS)) reasons.add(new Option(key, key));
    const button = (label: string, run: () => void) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.onclick = run;
      panel.append(b);
    };
    button("recording", () => {
      reset();
      micOpen(true);
      apply({ state: "recording" });
    });
    button("thinking", () => {
      reset();
      micOpen(false);
      apply({ state: "thinking" });
    });
    button("done", () => (reset(), settle({ state: "done" })));
    button("heard nothing", () => (reset(), settle({ state: "heard-nothing" })));
    button("failed", () => (reset(), settle(failure(reasons.value as Reason))));
    panel.append(reasons);
    button("hidden", () => (reset(), micOpen(false), apply({ state: "hidden" })));
    button("take", () => take(1800, 0, 300));
    // The mic stays open 600ms past key-up: the bars must outlive the key.
    button("tail take", () => take(1800, 600, 300));
    // Engine faster than the mic going quiet: "typing" ends the bars.
    button("fast take", () => take(900, 0, 40));
    document.body.append(panel);
  }

  // Pin one moment of one look. The look machine runs on real time (the
  // mic going quiet, thinking's minimum), so play the take up to the
  // wanted look first, then freeze the canvas and every CSS animation.
  const pinned = params.get("pill");
  if (pinned) {
    void (async () => {
      apply({ state: "recording" });
      for (let k = 0; k < 36; k++) pushLevel(voice(tick++)); // a voice to ease down from
      if (pinned !== "recording") {
        apply({ state: "thinking" });
        await wait(TIMING.micQuietMs + 50); // the mic goes quiet: thinking
      }
      if (pinned === "failed") apply(failure((params.get("reason") as Reason) ?? "unknown"));
      else if (pinned === "done" || pinned === "heard-nothing") apply({ state: pinned });
      if (pinned !== "recording" && pinned !== "thinking") await wait(TIMING.thinkingMinMs + 50);
      const at = Number(params.get("at") ?? 0);
      freeze(at);
      for (const animation of document.getAnimations()) {
        animation.currentTime = at;
        animation.pause();
      }
    })();
  }
}
