/// <reference types="vite/client" />
// The waveform overlay: sayit's one piece of visible UI, allowed by the
// north star because it answers "is this thing hearing me?" without the
// user looking away. Rust emits `mic_level` (peak 0..1) every 50ms while
// recording; this draws the last few seconds as bars sliding left —
// content, not decoration: the bars ARE the mic signal.
//
// After release the pill has a short life, so the wait and the outcome
// are visible too: thinking → done / heard nothing / failed → gone. The
// coordinator (main.ts) sends its word as `pill_state` — what the
// pipeline is doing — and decides when the window goes. This page decides
// what the pill SHOWS, from that word plus one piece of evidence only it
// has: whether mic levels are still arriving. The durations both sides
// agree on live in pill.ts.

import { listen } from "@tauri-apps/api/event";
import { HEARD_NOTHING, TIMING, lifeMs, type Pill, type Verdict } from "./pill";

const plate = document.getElementById("pill") as HTMLDivElement;
const canvas = document.getElementById("wave") as HTMLCanvasElement;
const ctx = canvas.getContext("2d")!;
const note = document.getElementById("note") as HTMLParagraphElement;
const noteText = document.getElementById("note-text") as HTMLSpanElement;
const motion = window.matchMedia("(prefers-reduced-motion: reduce)");

const BARS = 36;
const GOLD = "232, 176, 74";
/// A resting dot's height, as a share of the canvas height.
const REST = 0.1;
/// One pass of the thinking light, left to right.
const THINK_PERIOD_MS = 1400;

const levels: number[] = new Array(BARS).fill(0);
let lastLevelAt = -Infinity;
/// Bar heights at the moment the mic closed: the thinking light eases down
/// from the voice instead of snapping away from it.
let released: number[] = levels.slice();

/// What the pill draws. The coordinator's words map onto these, except
/// "typing", which looks exactly like thinking.
type Look = "hidden" | "recording" | "thinking" | Verdict["state"];

let word: Pill = { state: "hidden" }; // the coordinator's latest word
let look: Look = "hidden"; // what's on the plate right now
let lookSince = performance.now(); // when the current look began
let lookWord: Pill = word; // the word the current look was entered for
let recheck: number | undefined; // timer for a look that changes on its own
let running = false; // is the draw loop scheduled?
/// Dev switcher only (pill-dev.ts): render a fixed moment of the current
/// look instead of the live clock. Always null in the app.
let frozenAt: number | null = null;

function pushLevel(peak: number) {
  // Speech peaks live around 0.05–0.5; sqrt lifts quiet talkers into the
  // visible range without letting shouts clip past the pill.
  levels.push(Math.min(1, Math.sqrt(peak) * 1.5));
  levels.shift();
  lastLevelAt = performance.now();
  // A level after key-up is the mic still listening: re-judge the look.
  if (word.state === "thinking") update();
}

function apply(next: Pill) {
  // The level history belongs to one take: a new recording starts from a
  // flat line, not from the last take's bars.
  if (next.state === "recording") levels.fill(0);
  word = next;
  update();
}

// ---- the look: the word, corrected by the mic -------------------------

/// Mic honesty: while the coordinator says "thinking", the mic may still
/// be open (a release tail). Capture emits a level every 50ms, silence
/// included, for exactly as long as it listens — so fresh levels mean an
/// open mic, and an open mic gets the bars, never the thinking light.
const micOpen = (now: number) => now - lastLevelAt < TIMING.micQuietMs;

/// What the pill should show now — and, when that answer will change on
/// its own (the mic going quiet, thinking's minimum running out), the
/// moment to ask again.
function wantedLook(now: number): { next: Look; askAgainAt?: number } {
  switch (word.state) {
    case "hidden":
    case "recording":
      return { next: word.state };
    case "thinking":
      // Levels resuming after a stall bring the bars back (pushLevel asks
      // again): the truth wins over a smooth animation.
      return micOpen(now)
        ? { next: "recording", askAgainAt: lastLevelAt + TIMING.micQuietMs }
        : { next: "thinking" };
    case "typing":
      // stop_and_transcribe returned: the mic is closed, whatever the
      // levels did (a stray last tick can land just after the stop).
      return { next: "thinking" };
    default:
      // A verdict. If the take was still showing its bars, or its light
      // came on only just now, let thinking register first.
      if (look === "recording") return { next: "thinking", askAgainAt: now + TIMING.thinkingMinMs };
      if (look === "thinking" && now - lookSince < TIMING.thinkingMinMs) {
        return { next: "thinking", askAgainAt: lookSince + TIMING.thinkingMinMs };
      }
      return { next: word.state };
  }
}

/// Looks change on the coordinator's word, on a mic level after key-up,
/// or on a timer this sets for itself — never on animation frames, which
/// a busy or throttled webview can delay. Frames only paint.
function update() {
  const now = performance.now();
  const { next, askAgainAt } = wantedLook(now);
  clearTimeout(recheck);
  if (askAgainAt !== undefined) recheck = window.setTimeout(update, askAgainAt - now);
  // A verdict's look belongs to one word: a second failure in a row is
  // entered afresh (its own reason, flash, and exit clock) even though
  // the look's name is the same.
  const verdict = next === "done" || next === "heard-nothing" || next === "failed";
  if (next === look && !(verdict && lookWord !== word)) return;
  if (next === "thinking" && look === "recording") released = levels.slice();
  look = next;
  lookSince = now;
  lookWord = word;

  if (word.state === "heard-nothing" || word.state === "failed") {
    noteText.textContent = word.state === "failed" ? word.reason : HEARD_NOTHING;
  }
  note.hidden = next !== "heard-nothing" && next !== "failed";
  if (word.state === "done" || word.state === "heard-nothing" || word.state === "failed") {
    plate.style.setProperty("--life", `${lifeMs(word)}ms`);
    plate.style.setProperty("--fade", `${TIMING.fadeMs}ms`);
  }
  // Drop the attribute and force a style flush before setting it again:
  // CSS animations only restart when the value changes, and two failures
  // in a row must each get their own flash and their own exit clock.
  delete plate.dataset.state;
  void plate.offsetWidth;
  plate.dataset.state = next;

  if (!running && next !== "hidden") {
    running = true;
    requestAnimationFrame(draw);
  }
}

// ---- drawing ----------------------------------------------------------

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));
const mix = (a: number, b: number, k: number) => a + (b - a) * k;
const easeOut = (x: number) => 1 - (1 - x) ** 3;

/// One bar: height as a share of the canvas, alpha 0..1. The geometry is
/// the recording bars' own, so every look is drawn in the same hand.
function bar(i: number, height: number, alpha: number, w: number, h: number, dpr: number) {
  const gap = w / BARS;
  const barW = Math.max(2 * dpr, gap * 0.55);
  const barH = Math.max(2 * dpr, height * h);
  const x = i * gap + (gap - barW) / 2;
  const y = (h - barH) / 2;
  ctx.fillStyle = `rgba(${GOLD}, ${alpha})`;
  ctx.beginPath();
  ctx.roundRect(x, y, barW, barH, barW / 2);
  ctx.fill();
}

/// sayit gold, older bars fading — recency reads left-to-right.
const recordingAlpha = (i: number) => 0.35 + 0.65 * (i / BARS);

/// Thinking: a soft swell of light glides left to right through a resting
/// row of dots, once every THINK_PERIOD_MS — "working", without the
/// frantic. Still (reduced motion): the swell rests in the middle.
function thinkingLook(i: number, t: number, still: boolean) {
  const u = i / (BARS - 1);
  // The crest starts and ends off the pill, so each pass enters and leaves.
  const crest = still ? 0.5 : ((t % THINK_PERIOD_MS) / THINK_PERIOD_MS) * 1.6 - 0.3;
  const glow = Math.exp(-(((u - crest) / 0.14) ** 2));
  return { height: REST + 0.22 * glow, alpha: 0.3 + 0.6 * glow };
}

/// Done: the row lights up full gold and clears from the middle out, the
/// front brightest as it passes; the plate's own fade (CSS) finishes the
/// job by TIMING.doneMs. Still: the lit row, then gone.
function doneLook(i: number, t: number, still: boolean) {
  if (still) return { height: REST, alpha: 1 };
  const d = Math.abs(i / (BARS - 1) - 0.5) * 2; // 0 at the middle, 1 at the ends
  const front = easeOut(clamp01(t / 240)) * 1.15;
  const edge = Math.exp(-(((d - front) / 0.1) ** 2));
  const left = clamp01((d - front + 0.12) / 0.12); // 0 once the front has passed
  return { height: REST + 0.3 * edge, alpha: (0.75 + 0.25 * edge) * left };
}

function draw() {
  // performance.now(), not the frame's own timestamp: lookSince is
  // stamped in event handlers and timers, so one clock for both.
  const now = performance.now();
  if (look === "hidden") {
    // A hidden pill burns no frames; the next word restarts the loop.
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    running = false;
    return;
  }

  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth * dpr;
  const h = canvas.clientHeight * dpr;
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  ctx.clearRect(0, 0, w, h);

  const t = frozenAt ?? now - lookSince;
  const still = motion.matches;
  if (look === "recording") {
    for (let i = 0; i < BARS; i++) bar(i, levels[i], recordingAlpha(i), w, h, dpr);
  } else if (look === "thinking") {
    // Ease from the voice's last shape into the thinking row.
    const k = still ? 1 : easeOut(clamp01(t / 200));
    for (let i = 0; i < BARS; i++) {
      const light = thinkingLook(i, t, still);
      bar(i, mix(released[i], light.height, k), mix(recordingAlpha(i), light.alpha, k), w, h, dpr);
    }
  } else if (look === "done") {
    for (let i = 0; i < BARS; i++) {
      const light = doneLook(i, t, still);
      if (light.alpha > 0) bar(i, light.height, light.alpha, w, h, dpr);
    }
  }
  // heard-nothing and failed draw no bars: the message has the pill.
  requestAnimationFrame(draw);
}

// ---- wiring -----------------------------------------------------------

// Outside Tauri (plain `vite dev` in a browser) listen() has no backend.
// There, and only in dev, a state switcher drives the page instead; in a
// production build this branch is deleted, so pill-dev.ts never ships.
const inTauri = "__TAURI_INTERNALS__" in window;
if (inTauri) {
  void listen<number>("mic_level", (e) => pushLevel(e.payload));
  void listen<Pill>("pill_state", (e) => apply(e.payload));
} else if (import.meta.env.DEV) {
  void import("./pill-dev").then((dev) =>
    dev.mount({
      apply,
      pushLevel,
      freeze: (at) => {
        frozenAt = at;
      },
    }),
  );
}
