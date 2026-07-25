// The waveform overlay: sayit's one piece of visible UI, allowed by the
// north star because it answers "is this thing hearing me?" without the
// user looking away. Rust emits `mic_level` (peak 0..1) every 50ms while
// recording; this draws the last few seconds as bars sliding left —
// content, not decoration: the bars ARE the mic signal.

import { listen } from "@tauri-apps/api/event";

const canvas = document.getElementById("wave") as HTMLCanvasElement;
const ctx = canvas.getContext("2d")!;

const BARS = 36;
const levels: number[] = new Array(BARS).fill(0);

listen<number>("mic_level", (e) => {
  // Speech peaks live around 0.05–0.5; sqrt lifts quiet talkers into the
  // visible range without letting shouts clip past the pill.
  levels.push(Math.min(1, Math.sqrt(e.payload) * 1.5));
  levels.shift();
});

function draw() {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth * dpr;
  const h = canvas.clientHeight * dpr;
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  ctx.clearRect(0, 0, w, h);

  const gap = w / BARS;
  const barW = Math.max(2 * dpr, gap * 0.55);
  for (let i = 0; i < BARS; i++) {
    const level = levels[i];
    const barH = Math.max(2 * dpr, level * h);
    const x = i * gap + (gap - barW) / 2;
    const y = (h - barH) / 2;
    // sayit gold, older bars fading — recency reads left-to-right.
    ctx.fillStyle = `rgba(232, 176, 74, ${0.35 + 0.65 * (i / BARS)})`;
    ctx.beginPath();
    ctx.roundRect(x, y, barW, barH, barW / 2);
    ctx.fill();
  }
  requestAnimationFrame(draw);
}
draw();
