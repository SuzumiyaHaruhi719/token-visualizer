// Tray "active sessions" popover bootstrap.
//
// Shows ONE card per active session (project, model, tokens, cache hit %, est.
// cost) from `GET /api/sessions`, plus today's cost/cache from
// `GET /api/summary?range=today`. Live-updates via the SSE `sessions` event.
// When there are no sessions it shows a single "No active session" card.
// Rendered on a small frameless, transparent, always-on-top window created by
// src-tauri/src/windows.rs.
//
// WINDOW-SIZING CONTRACT (the Rust side sizes the window to match — keep these
// exact): each `.pop-card` is 84px tall, cards are separated by an 8px gap, and
// `#popover` keeps 6px padding all around. For N sessions the content height is
// `6 + N*84 + (N-1)*8 + 6`. At most MAX_CARDS cards render (the 6 most-recently
// active); the empty state renders as a single 84px card.

import "./popover.css";
import { getSessions, getSummary, subscribe } from "../lib/api";
import { formatTokens, formatPct, formatCost } from "../lib/format";
import { animateNumber } from "../lib/tween";
import type { SessionState, Summary, CmServerEvent } from "../lib/types";

/** Max cards rendered; the Rust side caps window height at the same count. */
const MAX_CARDS = 6;

/** Today's summary (for cache hit % + cost); refreshed on each sessions tick. */
let todaySummary: Summary | null = null;

/** Last token value rendered per session id, so live counts tween smoothly. */
const lastTokens = new Map<string, number>();

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

/** Escape a session id for use inside a CSS attribute selector. */
function cssAttrEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}

/** The 6 most-recently-active sessions, newest first. */
function topSessions(sessions: SessionState[]): SessionState[] {
  return [...sessions]
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, MAX_CARDS);
}

function cardMarkup(session: SessionState): string {
  const model = session.model.replace("claude-", "") || "—";
  const totals = todaySummary?.totals ?? null;
  const cachePct = formatPct(totals?.cacheHitRate ?? null);
  const todayCost = formatCost(totals?.costUsd ?? null);

  return `
    <div class="pop-card" data-session="${escapeHtml(session.sessionId)}">
      <div class="pop-head">
        <span class="pop-dot k-${escapeHtml(session.state.kind)}"></span>
        <span class="pop-project">${escapeHtml(session.project || "—")}</span>
        <span class="pop-model">${escapeHtml(model)}</span>
      </div>
      <div class="pop-grid">
        <div class="pop-metric">
          <span class="pop-metric-value" data-session="${escapeHtml(session.sessionId)}">${formatTokens(session.tokens)}</span>
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
    </div>
  `;
}

/** Render one card per active session, or a single empty-state card. */
function render(root: HTMLElement, sessions: SessionState[]): void {
  const shown = topSessions(sessions);
  if (!shown.length) {
    lastTokens.clear();
    root.innerHTML = `<div class="pop-card pop-empty-card"><div class="pop-empty">No active session</div></div>`;
    return;
  }

  root.innerHTML = shown.map(cardMarkup).join("");

  // Tween each card's token count from its remembered value (count up from 0 on
  // first appearance), and drop ids that are no longer active.
  const live = new Set(shown.map((s) => s.sessionId));
  for (const id of [...lastTokens.keys()]) {
    if (!live.has(id)) lastTokens.delete(id);
  }
  for (const s of shown) {
    const node = root.querySelector<HTMLElement>(
      `.pop-metric-value[data-session="${cssAttrEscape(s.sessionId)}"]`,
    );
    if (!node) continue;
    const from = lastTokens.get(s.sessionId) ?? 0;
    lastTokens.set(s.sessionId, s.tokens);
    animateNumber(node, from, s.tokens, { format: (v) => formatTokens(v) });
  }
}

async function bootstrap(): Promise<void> {
  const root = el("popover");

  // Initial paint from the one-shot getters.
  todaySummary = await getSummary("today");
  render(root, await getSessions());

  // Live updates: the `sessions` event carries the full active list; refresh
  // the today summary alongside it so cache % + cost track new activity.
  subscribe((ev: CmServerEvent) => {
    if (ev.type !== "sessions") return;
    const sessions = ev.data;
    void getSummary("today").then((s) => {
      todaySummary = s;
      render(root, sessions);
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

export { render, bootstrap, MAX_CARDS };
