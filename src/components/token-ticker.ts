// Live "number-ticker" module: a dedicated panel showing the EXACT token count
// for the selected range. A big total on top, per-model rows below — every
// number ticks (animates) to its new value when fresh usage arrives over SSE.
//
// Unlike the KPI strip (which uses the abbreviated formatTokens), this module
// shows the full integer with thousands separators (odometer style), exact to
// the ones digit. The host (main.ts) owns the fetch/SSE wiring and calls
// renderTokenTicker once, then updateTokenTicker on every range switch / live
// refresh. Per-element last values are tracked so updates tween from the prior
// exact value instead of snapping or counting up from 0.

import type { Summary, ByModel } from "../lib/types";
import { formatInt } from "../lib/format";
import { animateNumber } from "../lib/tween";

const TOTAL_KEY = "__total__";

// Last numeric value rendered into each ticking element, keyed by model name
// (the total uses TOTAL_KEY). Lets updates animate from the previous value.
const lastValues = new Map<string, number>();

function shortModel(model: string): string {
  return model.replace("claude-", "");
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
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

/**
 * Build the ticker panel markup into `container` from a (possibly null) summary.
 * Tolerates null totals (shows 0) and null/empty byModel (shows a muted note).
 * Resets remembered values so the first paint counts up from 0.
 */
export function renderTokenTicker(container: HTMLElement, summary: Summary | null): void {
  lastValues.clear();

  const models = safeModels(summary);
  const rows = models.length
    ? models
        .map(
          (m) => `
        <div class="ticker-row" data-model="${escapeHtml(m.model)}">
          <span class="ticker-model">${escapeHtml(shortModel(m.model))}</span>
          <span class="ticker-count" data-model="${escapeHtml(m.model)}">0</span>
        </div>`,
        )
        .join("")
    : `<div class="ticker-empty">no model data</div>`;

  container.innerHTML = `
    <div class="ticker-total-wrap">
      <span class="ticker-label">Total tokens · live</span>
      <span class="ticker-total" id="ticker-total">0</span>
    </div>
    <div class="ticker-models">${rows}</div>`;

  // Tween up from 0 to the initial values so the first paint animates in.
  updateTokenTicker(container, summary);
}

/**
 * Tween the total + per-model numbers toward the values in `summary`. Models
 * that appeared/disappeared between renders trigger a fresh markup rebuild so
 * the row set stays in sync; otherwise the numbers tick in place.
 */
export function updateTokenTicker(container: HTMLElement, summary: Summary | null): void {
  const models = safeModels(summary);

  // If the set of model rows no longer matches the DOM, rebuild markup first.
  const domRows = container.querySelectorAll<HTMLElement>(".ticker-row");
  const domModels = Array.from(domRows, (r) => r.dataset.model ?? "");
  const sameRows =
    domModels.length === models.length &&
    models.every((m, i) => m.model === domModels[i]);
  if (!sameRows) {
    renderTokenTicker(container, summary);
    return;
  }

  tweenTo(container.querySelector<HTMLElement>("#ticker-total"), TOTAL_KEY, safeTotal(summary));
  for (const m of models) {
    const sel = `.ticker-count[data-model="${cssEscape(m.model)}"]`;
    tweenTo(container.querySelector<HTMLElement>(sel), m.model, m.tokens);
  }
}

/** Tween one element from its remembered value to `to`, exact-integer format. */
function tweenTo(node: HTMLElement | null, key: string, to: number): void {
  if (!node) return;
  const from = lastValues.get(key) ?? 0;
  lastValues.set(key, to);
  animateNumber(node, from, to, { format: formatInt });
}

/** Escape a model name for use inside a CSS attribute selector. */
function cssEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}
