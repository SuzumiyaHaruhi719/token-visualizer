// Live "number-ticker" module: a dedicated panel showing the EXACT token count
// for the selected range. A big total on top, per-model rows below — every
// number ROLLS (mechanical-odometer style) to its new value when fresh usage
// arrives, whether from SSE or the 2Hz heartbeat in main.ts.
//
// Rows are reconciled BY MODEL KEY (not innerHTML-replaced), so when the model
// set changes on a tab switch the rows ENTER (expand + fade in) and LEAVE
// (collapse + fade out) smoothly, and the big-total reel is mounted ONCE and
// never re-created — it keeps rolling seamlessly across range switches.

import type { Summary, ByModel } from "../lib/types";
import { createOdometer, type OdometerHandle } from "./odometer";
import { revealOnNextFrame } from "../lib/motion";

// How a number update is applied to its odometer:
//  - "roll": slowly ease UP toward the value (live increments within a range).
//  - "snap": jump immediately, no animation (first paint).
//  - "transition": quick bidirectional roll (tab switch — seamless hand-off).
export type TickerUpdateMode = "roll" | "snap" | "transition";

/** Must match the CSS .ticker-row leave transition so we remove after it ends. */
const ROW_LEAVE_MS = 400;

/** Apply a value to an odometer with the requested mode. */
function applyValue(odo: OdometerHandle, n: number, mode: TickerUpdateMode): void {
  if (mode === "roll") odo.setTarget(n);
  else if (mode === "transition") odo.transitionTo(n);
  else odo.snapTo(n);
}

// Cached odometer handles for the currently mounted panel.
interface TickerState {
  total: OdometerHandle;
  byModel: Map<string, OdometerHandle>;
}

// One state per container, so multiple panels (tests) don't collide.
const states = new WeakMap<HTMLElement, TickerState>();

function shortModel(model: string): string {
  return model.replace("claude-", "");
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

/** Escape a model name for use inside a CSS attribute selector. */
function cssEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}

function safeModels(summary: Summary | null | undefined): ByModel[] {
  const raw = summary?.byModel;
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((m): m is ByModel => !!m && typeof m.model === "string")
    .map((m) => ({
      model: m.model,
      tokens: Number.isFinite(m.tokens) ? m.tokens : 0,
      costUsd: m.costUsd ?? null,
    }));
}

function safeTotal(summary: Summary | null | undefined): number {
  const t = summary?.totals?.tokens;
  return typeof t === "number" && Number.isFinite(t) ? t : 0;
}

const SHELL_MARKUP = `
    <div class="ticker-total-wrap">
      <span class="ticker-label">Total tokens · live</span>
      <span class="ticker-total" id="ticker-total"></span>
    </div>
    <div class="ticker-models"></div>`;

/** Build the panel shell + the (reels) total odometer, once. */
function buildShell(container: HTMLElement): TickerState {
  container.innerHTML = SHELL_MARKUP;
  const total = createOdometer({ reels: true });
  container.querySelector<HTMLElement>("#ticker-total")!.appendChild(total.el);
  const state: TickerState = { total, byModel: new Map() };
  states.set(container, state);
  return state;
}

/** A single per-model row element (starts in the `entering` state). */
function makeRow(model: string): HTMLElement {
  const row = document.createElement("div");
  row.className = "ticker-row entering";
  row.dataset.model = model;
  row.innerHTML =
    `<span class="ticker-model">${escapeHtml(shortModel(model))}</span>` +
    `<span class="ticker-count" data-model="${escapeHtml(model)}"></span>`;
  return row;
}

/**
 * Mount the ticker into `container` and roll up from 0 to the initial values.
 * Tolerates null totals (shows 0) and null/empty byModel (shows a muted note).
 */
export function renderTokenTicker(container: HTMLElement, summary: Summary | null): void {
  buildShell(container);
  sync(container, summary, "snap", undefined);
}

/**
 * Roll the total + per-model numbers toward the values in `summary`. New models
 * animate IN, departed models animate OUT; persisting rows roll in place. The
 * total reel is reused (never re-created). `totalOverride` lets the host drive a
 * perpetually-creeping total while the per-model rows track their real values.
 */
export function updateTokenTicker(
  container: HTMLElement,
  summary: Summary | null,
  mode: TickerUpdateMode = "roll",
  totalOverride?: number,
): void {
  // (Re)build the shell if it's missing (first call, or the host replaced it).
  if (!states.get(container) || !container.querySelector("#ticker-total")) {
    buildShell(container);
    mode = "snap";
  }
  sync(container, summary, mode, totalOverride);
}

function sync(
  container: HTMLElement,
  summary: Summary | null,
  mode: TickerUpdateMode,
  totalOverride: number | undefined,
): void {
  const state = states.get(container)!;
  applyValue(state.total, totalOverride ?? safeTotal(summary), mode);
  syncRows(container, state, safeModels(summary), mode);
}

/** Reconcile the per-model rows by key: enter new, leave departed, roll persisting. */
function syncRows(
  container: HTMLElement,
  state: TickerState,
  models: ByModel[],
  mode: TickerUpdateMode,
): void {
  const wrap = container.querySelector<HTMLElement>(".ticker-models");
  if (!wrap) return;

  // Empty state: animate any rows out and show the muted note.
  if (!models.length) {
    for (const row of wrap.querySelectorAll<HTMLElement>(".ticker-row:not(.leaving)")) {
      leaveRow(row, state);
    }
    if (!wrap.querySelector(".ticker-empty")) {
      const note = document.createElement("div");
      note.className = "ticker-empty";
      note.textContent = "no model data";
      wrap.appendChild(note);
    }
    return;
  }
  wrap.querySelector(".ticker-empty")?.remove();

  const desired = new Set(models.map((m) => m.model));

  // LEAVE: rows whose model is gone collapse + fade, then are removed.
  for (const row of wrap.querySelectorAll<HTMLElement>(".ticker-row:not(.leaving)")) {
    if (!desired.has(row.dataset.model ?? "")) leaveRow(row, state);
  }

  // ENTER / persist, kept in the summary's (token-desc) order.
  let anchor: HTMLElement | null = null;
  for (const m of models) {
    let row = wrap.querySelector<HTMLElement>(
      `.ticker-row[data-model="${cssEscape(m.model)}"]:not(.leaving)`,
    );
    if (!row) {
      row = makeRow(m.model);
      const odo = createOdometer();
      row.querySelector<HTMLElement>(".ticker-count")!.appendChild(odo.el);
      state.byModel.set(m.model, odo);
      placeAfter(wrap, row, anchor);
      revealOnNextFrame(row, "entering"); // expand + fade in
    } else {
      placeAfter(wrap, row, anchor);
    }
    const odo = state.byModel.get(m.model);
    if (odo) applyValue(odo, m.tokens, mode);
    anchor = row;
  }
}

/** Move `row` to sit right after `anchor` (or first) only if it isn't already. */
function placeAfter(wrap: HTMLElement, row: HTMLElement, anchor: HTMLElement | null): void {
  if (anchor) {
    if (anchor.nextElementSibling !== row) anchor.after(row);
  } else if (wrap.firstElementChild !== row) {
    wrap.prepend(row);
  }
}

/** Collapse + fade a row out, dropping its odometer handle, then remove it. */
function leaveRow(row: HTMLElement, state: TickerState): void {
  if (row.classList.contains("leaving")) return;
  state.byModel.delete(row.dataset.model ?? "");
  row.classList.remove("entering");
  row.classList.add("leaving");
  if (typeof window !== "undefined" && typeof window.setTimeout === "function") {
    window.setTimeout(() => row.remove(), ROW_LEAVE_MS);
  } else {
    row.remove();
  }
}
