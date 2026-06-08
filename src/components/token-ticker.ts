// Live "number-ticker" module: a dedicated panel showing the EXACT token count
// for the selected range. A big total on top, per-model rows below — every
// number ROLLS (mechanical-odometer style) to its new value when fresh usage
// arrives, whether from SSE or the 2Hz heartbeat in main.ts.
//
// Unlike the KPI strip (which uses the abbreviated formatTokens), this module
// shows the full integer with thousands separators, exact to the ones digit.
// The host (main.ts) owns the fetch/SSE wiring and calls renderTokenTicker once,
// then updateTokenTicker on every range switch / live refresh. Odometer handles
// are cached per element (total + per-model) and REUSED across updates so each
// number rolls from its prior value instead of rebuilding/snapping.

import type { Summary, ByModel } from "../lib/types";
import { createOdometer, type OdometerHandle } from "./odometer";

// How a number update is applied to its odometer:
//  - "roll": slowly ease UP toward the value (live increments within a range).
//  - "snap": jump immediately, no animation (first paint / structural rebuild).
//  - "transition": quick bidirectional ~1s roll (tab switch — seamless hand-off
//    to the new range's total, no freeze).
export type TickerUpdateMode = "roll" | "snap" | "transition";

/** Apply a value to an odometer with the requested mode. */
function applyValue(odo: OdometerHandle, n: number, mode: TickerUpdateMode): void {
  if (mode === "roll") {
    odo.setTarget(n);
  } else if (mode === "transition") {
    odo.transitionTo(n);
  } else {
    odo.snapTo(n);
  }
}

// Cached odometer handles for the currently mounted panel. The total uses
// `total`; per-model handles are keyed by model name. Reset on full rebuild.
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
 * Mounts fresh odometers and rolls them up from 0 to the initial values.
 */
export function renderTokenTicker(container: HTMLElement, summary: Summary | null): void {
  const models = safeModels(summary);

  const rows = models.length
    ? models
        .map(
          (m) => `
        <div class="ticker-row" data-model="${escapeHtml(m.model)}">
          <span class="ticker-model">${escapeHtml(shortModel(m.model))}</span>
          <span class="ticker-count" data-model="${escapeHtml(m.model)}"></span>
        </div>`,
        )
        .join("")
    : `<div class="ticker-empty">no model data</div>`;

  container.innerHTML = `
    <div class="ticker-total-wrap">
      <span class="ticker-label">Total tokens · live</span>
      <span class="ticker-total" id="ticker-total"></span>
    </div>
    <div class="ticker-models">${rows}</div>`;

  // Mount a fresh odometer for the total (slot-machine reels) and one plain
  // odometer per model row.
  const total = createOdometer({ reels: true });
  container.querySelector<HTMLElement>("#ticker-total")!.appendChild(total.el);

  const byModel = new Map<string, OdometerHandle>();
  for (const m of models) {
    const slot = container.querySelector<HTMLElement>(
      `.ticker-count[data-model="${cssEscape(m.model)}"]`,
    );
    if (!slot) continue;
    const odo = createOdometer();
    slot.appendChild(odo.el);
    byModel.set(m.model, odo);
  }

  states.set(container, { total, byModel });

  // First paint: snap to the initial values (no multi-second roll on mount).
  updateTokenTicker(container, summary, "snap");
}

/**
 * Roll the total + per-model numbers toward the values in `summary`. Models
 * that appeared/disappeared between renders trigger a markup rebuild (preserving
 * the odometer handles of models that persist, so they roll rather than snap);
 * otherwise the numbers roll in place.
 */
export function updateTokenTicker(
  container: HTMLElement,
  summary: Summary | null,
  mode: TickerUpdateMode = "roll",
  totalOverride?: number,
): void {
  const models = safeModels(summary);
  const state = states.get(container);

  // If there's no cached state, or the set of model rows no longer matches the
  // DOM, rebuild markup (preserving persisting model odometers) first. A model
  // set change is a structural shift, so snap the values into place.
  const domRows = container.querySelectorAll<HTMLElement>(".ticker-row");
  const domModels = Array.from(domRows, (r) => r.dataset.model ?? "");
  const sameRows =
    !!state &&
    domModels.length === models.length &&
    models.every((m, i) => m.model === domModels[i]);

  if (!sameRows) {
    rebuildPreserving(container, summary, models);
    return;
  }

  // `totalOverride` lets the host drive a perpetually-creeping total (always
  // rolling) while the per-model rows track their real values.
  applyValue(state.total, totalOverride ?? safeTotal(summary), mode);
  for (const m of models) {
    const odo = state.byModel.get(m.model);
    if (odo) applyValue(odo, m.tokens, mode);
  }
}

/**
 * Rebuild the panel markup for a changed model set, re-using odometer handles
 * for models that persist so they keep rolling from their prior value. New
 * models get fresh odometers; departed models are dropped.
 */
function rebuildPreserving(
  container: HTMLElement,
  summary: Summary | null,
  models: ByModel[],
): void {
  const prev = states.get(container);
  const prevByModel = prev?.byModel ?? new Map<string, OdometerHandle>();

  const rows = models.length
    ? models
        .map(
          (m) => `
        <div class="ticker-row" data-model="${escapeHtml(m.model)}">
          <span class="ticker-model">${escapeHtml(shortModel(m.model))}</span>
          <span class="ticker-count" data-model="${escapeHtml(m.model)}"></span>
        </div>`,
        )
        .join("")
    : `<div class="ticker-empty">no model data</div>`;

  container.innerHTML = `
    <div class="ticker-total-wrap">
      <span class="ticker-label">Total tokens · live</span>
      <span class="ticker-total" id="ticker-total"></span>
    </div>
    <div class="ticker-models">${rows}</div>`;

  // Re-use the total odometer if we had one; otherwise create it (reels).
  const total = prev?.total ?? createOdometer({ reels: true });
  container.querySelector<HTMLElement>("#ticker-total")!.appendChild(total.el);

  const byModel = new Map<string, OdometerHandle>();
  for (const m of models) {
    const slot = container.querySelector<HTMLElement>(
      `.ticker-count[data-model="${cssEscape(m.model)}"]`,
    );
    if (!slot) continue;
    const odo = prevByModel.get(m.model) ?? createOdometer();
    slot.appendChild(odo.el);
    byModel.set(m.model, odo);
  }

  states.set(container, { total, byModel });

  // Structural rebuild: snap values into place rather than rolling.
  total.snapTo(safeTotal(summary));
  for (const m of models) {
    byModel.get(m.model)?.snapTo(m.tokens);
  }
}

/** Escape a model name for use inside a CSS attribute selector. */
function cssEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}
