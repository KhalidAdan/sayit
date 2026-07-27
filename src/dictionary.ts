// The dictionary editor. Rows of misheard → corrected pairs, autosaved as
// you type (no Save button to forget), with a live "try it" preview. The
// preview calls Rust's real apply() — one algorithm, nothing to drift.
//
// The local `rules` array is the UI's working model: it may hold
// half-finished rows (empty fields) that render but are never saved.

import { invoke } from "@tauri-apps/api/core";

type Rule = { from: string; to: string };

// Outside Tauri (plain `vite dev` in a browser) invoke() has no backend.
// This shim keeps the page workable for visual dev: rules live in memory
// and the preview uses a rough JS approximation of the Rust matcher.
const inTauri = "__TAURI_INTERNALS__" in window;
const backend = {
  load: (): Promise<Rule[]> =>
    inTauri ? invoke<Rule[]>("dictionary_rules") : Promise.resolve([]),
  save: (replacements: Rule[]): Promise<void> =>
    inTauri ? invoke("dictionary_save", { replacements }) : Promise.resolve(),
  preview: (replacements: Rule[], text: string): Promise<string> => {
    if (inTauri) return invoke<string>("dictionary_preview", { replacements, text });
    let out = text;
    for (const r of replacements) {
      const safe = r.from.trim().replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      if (safe) out = out.replace(new RegExp(`\\b${safe}\\b`, "gi"), r.to);
    }
    return Promise.resolve(out);
  },
  hide: (): Promise<void> => (inTauri ? invoke("dictionary_hide") : Promise.resolve()),
};

const list = document.getElementById("rules") as HTMLUListElement;
const addButton = document.getElementById("add") as HTMLButtonElement;
const tryInput = document.getElementById("try") as HTMLInputElement;
const previewOut = document.getElementById("preview") as HTMLParagraphElement;
const savedFlag = document.getElementById("saved") as HTMLParagraphElement;

let rules: Rule[] = [];

// Only complete rows leave the window; blanks are drafts, not data.
const complete = () => rules.filter((r) => r.from.trim().length > 0);

// ---- persistence ----------------------------------------------------

let saveTimer: number | undefined;
let savedTimer: number | undefined;

function saveSoon() {
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(async () => {
    await backend.save(complete());
    savedFlag.style.opacity = "1";
    clearTimeout(savedTimer);
    savedTimer = window.setTimeout(() => (savedFlag.style.opacity = "0"), 1500);
  }, 300);
}

let previewTimer: number | undefined;

function previewSoon() {
  clearTimeout(previewTimer);
  previewTimer = window.setTimeout(async () => {
    const text = tryInput.value;
    if (text.trim().length === 0) {
      previewOut.textContent = "The corrected text appears here.";
      previewOut.classList.replace("text-zinc-100", "text-zinc-500");
      return;
    }
    previewOut.textContent = await backend.preview(complete(), text);
    previewOut.classList.replace("text-zinc-500", "text-zinc-100");
  }, 120);
}

// ---- rows -----------------------------------------------------------

const INPUT_CLASSES =
  "min-w-0 flex-1 rounded-md bg-white/5 px-2.5 py-1.5 text-sm text-zinc-100 " +
  "-outline-offset-1 inset-ring inset-ring-white/10 placeholder:text-zinc-600 " +
  "focus-visible:outline-2 focus-visible:outline-blue-500";

function rowEl(rule: Rule, index: number): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "flex items-center gap-2";
  li.innerHTML = `
    <input name="from" aria-label="Misheard word" placeholder="misheard"
      autocomplete="off" spellcheck="false" class="${INPUT_CLASSES}" />
    <svg viewBox="0 0 16 16" class="size-4 shrink-0 fill-zinc-600" aria-hidden="true">
      <path fill-rule="evenodd" clip-rule="evenodd"
        d="M2 8a.75.75 0 0 1 .75-.75h8.69L8.22 4.03a.75.75 0 0 1 1.06-1.06l4.5 4.5a.75.75 0 0 1 0 1.06l-4.5 4.5a.75.75 0 0 1-1.06-1.06l3.22-3.22H2.75A.75.75 0 0 1 2 8Z" />
    </svg>
    <input name="to" aria-label="Replace with" placeholder="corrected"
      autocomplete="off" spellcheck="false" class="${INPUT_CLASSES}" />
    <button type="button" aria-label="Delete rule"
      class="group relative shrink-0 rounded-md p-1.5 hover:bg-white/5 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-500">
      <svg viewBox="0 0 16 16" class="size-4 fill-zinc-500 group-hover:fill-zinc-300" aria-hidden="true">
        <path
          d="M5.28 4.22a.75.75 0 0 0-1.06 1.06L6.94 8l-2.72 2.72a.75.75 0 1 0 1.06 1.06L8 9.06l2.72 2.72a.75.75 0 1 0 1.06-1.06L9.06 8l2.72-2.72a.75.75 0 0 0-1.06-1.06L8 6.94 5.28 4.22Z" />
      </svg>
      <span class="absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2 pointer-fine:hidden" aria-hidden="true"></span>
    </button>`;

  const [from, to] = Array.from(li.querySelectorAll("input"));
  const del = li.querySelector("button")!;
  from.value = rule.from;
  to.value = rule.to;

  for (const [input, key] of [
    [from, "from"],
    [to, "to"],
  ] as const) {
    input.addEventListener("input", () => {
      rules[index][key] = input.value;
      saveSoon();
      previewSoon();
    });
  }
  // Enter walks the natural path: misheard → corrected → next rule.
  from.addEventListener("keydown", (e) => {
    if (e.key === "Enter") to.focus();
  });
  to.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    if (index === rules.length - 1) addRow();
    else list.querySelectorAll("input")[(index + 1) * 2]?.focus();
  });
  del.addEventListener("click", () => {
    rules.splice(index, 1);
    if (rules.length === 0) rules.push({ from: "", to: "" });
    render();
    saveSoon();
    previewSoon();
  });
  return li;
}

function render() {
  list.replaceChildren(...rules.map(rowEl));
}

function addRow() {
  rules.push({ from: "", to: "" });
  render();
  const inputs = list.querySelectorAll("input");
  inputs[inputs.length - 2]?.focus();
}

// ---- boot -----------------------------------------------------------

addButton.addEventListener("click", addRow);
tryInput.addEventListener("input", previewSoon);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") void backend.hide();
});

void backend.load().then((saved) => {
  rules = saved;
  // An empty dictionary greets you with a row to fill, not a blank void.
  if (rules.length === 0) rules.push({ from: "", to: "" });
  render();
});
