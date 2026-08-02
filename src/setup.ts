import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Snapshot = {
  firstRun: boolean;
  running: boolean;
  ready: boolean;
  keyboardOk: boolean;
  keyboardDetail: string;
  uinputOk: boolean;
  uinputDetail: string;
  microphoneOk: boolean;
  microphoneDetail: string;
  assetsOk: boolean;
  enginePath: string;
  modelPath: string;
  error: string | null;
};

type Progress = {
  asset: "engine" | "model";
  downloaded: number;
  total: number;
  phase: "downloading" | "verifying" | "ready";
};

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const prepare = $<HTMLButtonElement>("prepare");
const finish = $<HTMLButtonElement>("finish");
const close = $<HTMLButtonElement>("close");
const error = $<HTMLParagraphElement>("error");
const target = $<HTMLInputElement>("paste-target");
const progress = document.querySelector<HTMLDivElement>(".progress")!;
const progressBar = $<HTMLSpanElement>("progress-bar");
const progressCopy = $<HTMLParagraphElement>("progress-copy");

function check(name: string, ok: boolean, detail: string) {
  const row = document.querySelector<HTMLElement>(`[data-check="${name}"]`)!;
  row.classList.toggle("ok", ok);
  row.classList.toggle("fail", !ok);
  $(`${name}-detail`).textContent = detail;
  $(`${name}-state`).textContent = ok ? "Ready" : "Needed";
}

function formatBytes(bytes: number) {
  return `${(bytes / 1024 / 1024).toFixed(bytes > 100 * 1024 * 1024 ? 0 : 1)} MB`;
}

async function refresh() {
  const state = await invoke<Snapshot>("setup_snapshot");
  $("title").textContent = state.firstRun ? "Teach sayit this machine." : "This machine, inspected.";
  check("keyboard", state.keyboardOk, state.keyboardDetail);
  check("uinput", state.uinputOk, state.uinputDetail);
  check("microphone", state.microphoneOk, state.microphoneDetail);
  check(
    "engine",
    state.assetsOk || state.ready,
    state.assetsOk
      ? `Whisper small · ${state.modelPath}`
      : "Whisper small · downloads once, then stays here",
  );
  prepare.hidden = state.ready;
  prepare.textContent = state.running ? "Preparing…" : state.assetsOk ? "Warm the engine" : "Prepare this machine";
  prepare.disabled = state.running || !state.keyboardOk || !state.uinputOk || !state.microphoneOk;
  finish.hidden = !state.ready;
  finish.textContent = state.firstRun ? "Start sayit" : "Done";
  close.textContent = state.firstRun ? "Not now" : "Close";
  error.hidden = state.error === null;
  error.textContent = state.error ?? "";
}

prepare.addEventListener("click", async () => {
  prepare.disabled = true;
  prepare.textContent = "Preparing…";
  error.hidden = true;
  try {
    await invoke("setup_begin");
  } catch (cause) {
    error.textContent = String(cause);
    error.hidden = false;
  }
  await refresh();
});

finish.addEventListener("click", async () => {
  try {
    await invoke("setup_finish");
  } catch (cause) {
    error.textContent = String(cause);
    error.hidden = false;
  }
});
close.addEventListener("click", () => void invoke("setup_hide"));

$("test-paste").addEventListener("click", async () => {
  target.value = "";
  target.focus();
  try {
    await invoke("setup_test_injection");
  } catch (cause) {
    error.textContent = String(cause);
    error.hidden = false;
  }
});
$("test-sound").addEventListener("click", () => void invoke("play_sound", { slot: "press" }));

listen<Progress>("setup_progress", ({ payload }) => {
  progress.hidden = false;
  const ratio = payload.total === 0 ? 0 : payload.downloaded / payload.total;
  progressBar.style.width = `${Math.min(100, ratio * 100)}%`;
  progressCopy.textContent = `${payload.asset} · ${payload.phase} · ${formatBytes(payload.downloaded)} / ${formatBytes(payload.total)}`;
});
listen("setup_ready", () => void refresh());
listen<string>("setup_failed", ({ payload }) => {
  error.textContent = payload;
  error.hidden = false;
  void refresh();
});

// The keycaps are explanatory, not another global listener: setup must never
// collect ordinary typed keys in the webview either.
document.addEventListener("keydown", (event) => {
  if (event.key === "Control" && event.location === KeyboardEvent.DOM_KEY_LOCATION_LEFT) {
    document.querySelector<HTMLElement>("kbd:not(.live)")?.classList.add("live");
  }
});
document.addEventListener("keyup", (event) => {
  if (event.key === "Control") {
    const live = document.querySelectorAll<HTMLElement>("kbd.live");
    window.setTimeout(() => live.forEach((key) => key.classList.remove("live")), 120);
  }
});

void refresh();
