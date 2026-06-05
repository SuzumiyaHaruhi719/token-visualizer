// Tray "current session" popover bootstrap.
//
// Shows the CURRENT session (project, model, tokens, cache hit %, est. cost)
// from `GET /api/current`, plus today's cost from `GET /api/summary?range=today`.
// Live-updates the session via the SSE `usage` event. When there is no current
// session it shows "No active session". Rendered on a small frameless,
// transparent, always-on-top window created by src-tauri/src/windows.rs.

import "./popover.css";
import { getCurrent, getSummary, subscribe } from "../lib/api";
import { formatTokens, formatPct, formatCost } from "../lib/format";
import type { SessionState, Summary, CmServerEvent } from "../lib/types";

/** Today's summary (for cache hit % + cost); refreshed on each usage tick. */
let todaySummary: Summary | null = null;

function el<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`#${id} not found`);
  return node as T;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

/** Render the current-session card, or the empty state when there is none. */
function render(root: HTMLElement, session: SessionState | null): void {
  if (!session) {
    root.innerHTML = `<div class="pop-empty">No active session</div>`;
    return;
  }

  const model = session.model.replace("claude-", "") || "—";
  const totals = todaySummary?.totals ?? null;
  const cachePct = formatPct(totals?.cacheHitRate ?? null);
  const todayCost = formatCost(totals?.costUsd ?? null);

  root.innerHTML = `
    <div class="pop-card">
      <div class="pop-head">
        <span class="pop-dot k-${escapeHtml(session.state.kind)}"></span>
        <span class="pop-project">${escapeHtml(session.project || "—")}</span>
        <span class="pop-model">${escapeHtml(model)}</span>
      </div>
      <div class="pop-grid">
        <div class="pop-metric">
          <span class="pop-metric-value">${formatTokens(session.tokens)}</span>
          <span class="pop-metric-label">Tokens</span>
        </div>
        <div class="pop-metric">
          <span class="pop-metric-value">${cachePct}</span>
          <span class="pop-metric-label">Cache hit</span>
        </div>
        <div class="pop-metric">
          <span class="pop-metric-value">${todayCost}</span>
          <span class="pop-metric-label">Cost</span>
        </div>
      </div>
      <div class="pop-foot">Today · est. cost <strong>${todayCost}</strong></div>
    </div>
  `;
}

async function bootstrap(): Promise<void> {
  const root = el("popover");

  // Initial paint from the one-shot getters.
  todaySummary = await getSummary("today");
  render(root, await getCurrent());

  // Live updates: the `usage` event carries the current session; refresh the
  // today summary alongside it so cache % + cost track new activity.
  subscribe((ev: CmServerEvent) => {
    if (ev.type !== "usage") return;
    void getSummary("today").then((s) => {
      todaySummary = s;
      render(root, ev.data.current);
    });
  });
}

function autostart(): void {
  // Only auto-boot a real, empty popover mount. Lets tests import this module
  // and call render() without side effects.
  const mount =
    typeof document !== "undefined" ? document.getElementById("popover") : null;
  if (mount && mount.childElementCount === 0) void bootstrap();
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    window.addEventListener("DOMContentLoaded", autostart);
  } else {
    autostart();
  }
}

export { render, bootstrap };
