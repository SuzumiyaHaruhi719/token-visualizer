// "Session limits" panel renderer: per-source frosted cards for Claude and
// Codex. Claude shows the current session + a muted note (no local rate-limit
// data). Codex shows the session plus 5h / Weekly gauge bars with live reset
// countdowns. Pure DOM string builders + a countdown refresher; the host
// (main.ts) owns the fetch/SSE wiring and the 1s timer tick.

import type { Limits, RateWindow } from "../lib/types";
import { formatTokens } from "../lib/format";

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

function shortModel(model: string): string {
  return model.replace("claude-", "");
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return value < 0 ? 0 : value > 100 ? 100 : value;
}

/** Human countdown from now until `resetsAt` (epoch SECONDS): "2h 14m", "45s". */
export function formatCountdown(resetsAtSeconds: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(resetsAtSeconds)) return "—";
  let secs = Math.floor(resetsAtSeconds - nowMs / 1000);
  if (secs <= 0) return "now";
  const days = Math.floor(secs / 86400);
  secs -= days * 86400;
  const hours = Math.floor(secs / 3600);
  secs -= hours * 3600;
  const mins = Math.floor(secs / 60);
  secs -= mins * 60;

  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${mins}m`;
  if (mins > 0) return `${mins}m ${secs}s`;
  return `${secs}s`;
}

function gaugeMarkup(label: string, win: RateWindow | null): string {
  if (!win) {
    return `
      <div class="limit-gauge">
        <div class="limit-gauge-head"><span class="limit-gauge-label">${label}</span><span class="limit-gauge-left">—</span></div>
        <div class="limit-bar"><div class="limit-bar-fill" style="width:0%"></div></div>
        <div class="limit-reset">no data</div>
      </div>`;
  }
  const used = clampPercent(win.usedPercent);
  const left = clampPercent(win.remainingPercent);
  // Render the fill at its FINAL width directly (no grow-from-0). The panel is
  // re-rendered whenever live data lands; animating 0->used on every refresh read
  // as the gauge "constantly auto-refreshing", so it now paints statically and
  // changes only when the percentage actually does.
  return `
    <div class="limit-gauge" data-resets-at="${win.resetsAt}">
      <div class="limit-gauge-head">
        <span class="limit-gauge-label">${label}</span>
        <span class="limit-gauge-left">${Math.round(left)}% left</span>
      </div>
      <div class="limit-bar"><div class="limit-bar-fill" style="width:${used}%"></div></div>
      <div class="limit-reset">resets in <span class="limit-countdown">${formatCountdown(win.resetsAt)}</span></div>
    </div>`;
}

function claudeCard(c: Limits["claude"]): string {
  const session = c.session
    ? `<div class="limit-session">
         <span class="limit-proj">${escapeHtml(c.session.project)}</span>
         <span class="limit-sep">·</span>
         <span class="limit-model">${escapeHtml(shortModel(c.session.model))}</span>
         <span class="limit-sep">·</span>
         <span class="limit-tok">${formatTokens(c.session.tokens)} tok</span>
       </div>`
    : `<div class="limit-session limit-empty">no active session</div>`;

  return `
    <div class="limit-card limit-card-claude">
      <div class="limit-head">
        <span class="limit-name">Claude</span>
      </div>
      ${session}
      <div class="limit-note">${escapeHtml(c.note || "—")}</div>
    </div>`;
}

function codexCard(c: Limits["codex"]): string {
  const session = c.session
    ? `<div class="limit-session">
         <span class="limit-model">${escapeHtml(shortModel(c.session.model))}</span>
         <span class="limit-sep">·</span>
         <span class="limit-tok">${formatTokens(c.session.tokens)} tok</span>
       </div>`
    : `<div class="limit-session limit-empty">no active session</div>`;

  const plan = c.planType
    ? `<span class="limit-chip">${escapeHtml(c.planType)}</span>`
    : "";

  return `
    <div class="limit-card limit-card-codex">
      <div class="limit-head">
        <span class="limit-name">Codex</span>
        ${plan}
      </div>
      ${session}
      <div class="limit-gauges">
        ${gaugeMarkup("5h", c.fiveHour)}
        ${gaugeMarkup("Weekly", c.weekly)}
      </div>
    </div>`;
}

/** Render the full Session limits panel body into `container`. */
export function renderLimits(container: HTMLElement, limits: Limits): void {
  container.innerHTML = `
    <div class="limits-grid">
      ${claudeCard(limits.claude)}
      ${codexCard(limits.codex)}
    </div>`;
}

/**
 * Recompute the visible countdown text for every gauge in `container` without
 * re-rendering markup. Called once per second by the host.
 */
export function tickCountdowns(container: HTMLElement, nowMs: number = Date.now()): void {
  const gauges = container.querySelectorAll<HTMLElement>(".limit-gauge[data-resets-at]");
  gauges.forEach((g) => {
    const raw = g.getAttribute("data-resets-at");
    const span = g.querySelector<HTMLElement>(".limit-countdown");
    if (!raw || !span) return;
    const resetsAt = Number(raw);
    span.textContent = formatCountdown(resetsAt, nowMs);
  });
}
