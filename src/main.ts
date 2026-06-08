// Dashboard bootstrap: KPIs + ECharts + current-session strip, fed by the
// HTTP API and live SSE stream (with mock fallback).

import * as echarts from "echarts";
import "./styles.css";
import {
  getSummary,
  getSessions,
  getLimits,
  subscribe,
  getSettings,
  updateSettings,
} from "./lib/api";
import { createSettingsPanel } from "./components/settings-panel";
import { formatTokens, formatCost, formatPct, formatInt } from "./lib/format";
import { animateNumber } from "./lib/tween";
import { renderLimits, tickCountdowns } from "./components/limits";
import { renderBySource } from "./components/by-source";
import { renderTokenTicker, updateTokenTicker } from "./components/token-ticker";
import type {
  Summary,
  SessionState,
  RangeKey,
  CmServerEvent,
  Totals,
  BySource,
  Limits,
} from "./lib/types";

const RANGES: { key: RangeKey; label: string }[] = [
  { key: "today", label: "Today" },
  { key: "7d", label: "7d" },
  { key: "30d", label: "30d" },
  { key: "all", label: "All" },
];

const COLORS = {
  input: "#da7757",
  output: "#e0a96d",
  cacheCreate: "#7c93c3",
  cacheRead: "#5bc0a8",
  axis: "#3a3a3a",
  label: "#9a9a9a",
};

type Charts = {
  timeseries: echarts.ECharts;
  donut: echarts.ECharts;
  projects: echarts.ECharts;
};

let currentRange: RangeKey = "today";
let charts: Charts | null = null;

function el<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`#${id} not found`);
  return node as T;
}

function renderShell(root: HTMLElement): void {
  root.innerHTML = `
    <header class="topbar">
      <div class="brand"><span class="hex">⬡</span> Claude Monitor</div>
      <nav class="range-tabs" id="range-tabs">
        ${RANGES.map(
          (r, i) =>
            `<button class="range-tab${i === 0 ? " active" : ""}" data-range="${r.key}">${r.label}</button>`,
        ).join("")}
      </nav>
      <div class="topbar-right">
        <div class="live" id="live"><span class="dot"></span> live</div>
        <button class="settings-gear" id="settings-gear" aria-label="Settings" title="Settings">⚙</button>
      </div>
    </header>

    <div id="settings-mount"></div>

    <div class="import-bar" id="import-bar" hidden>
      <div class="import-fill" id="import-fill"></div>
      <span class="import-text" id="import-text"></span>
    </div>

    <section class="kpis">
      <div class="kpi"><div class="kpi-label">Total Tokens</div><div class="kpi-value" id="kpi-tokens">—</div><div class="kpi-sub" id="kpi-tokens-sub"></div></div>
      <div class="kpi"><div class="kpi-label">Est. Cost</div><div class="kpi-value" id="kpi-cost">—</div><div class="kpi-sub">estimated · API list price</div></div>
      <div class="kpi"><div class="kpi-label">Cache Hit Rate</div><div class="kpi-value" id="kpi-cache">—</div><div class="kpi-sub" id="kpi-cache-sub"></div></div>
      <div class="kpi"><div class="kpi-label">Sessions / Messages</div><div class="kpi-value" id="kpi-sessions">—</div><div class="kpi-sub" id="kpi-sessions-sub"></div></div>
    </section>

    <section class="bysource" id="bysource">
      <span class="bysource-empty">No source data</span>
    </section>

    <section class="panel panel-wide ticker-panel" id="token-ticker"></section>

    <section class="panel panel-wide">
      <h2 class="panel-title">Token usage over time</h2>
      <div class="chart" id="chart-timeseries"></div>
    </section>

    <div class="grid-2">
      <section class="panel">
        <h2 class="panel-title">By model</h2>
        <div class="chart chart-sm" id="chart-donut"></div>
      </section>
      <section class="panel">
        <h2 class="panel-title">Top projects</h2>
        <div class="chart chart-sm" id="chart-projects"></div>
      </section>
    </div>

    <section class="panel">
      <h2 class="panel-title">Session limits</h2>
      <div id="limits-body">
        <span class="cs-empty">Loading limits…</span>
      </div>
    </section>

    <section class="panel">
      <h2 class="panel-title">Cache efficiency</h2>
      <div class="cache-panel">
        <div class="cache-big" id="cache-big">—</div>
        <div class="cache-split">
          <div class="cache-row"><span class="swatch" style="background:${COLORS.cacheRead}"></span>Cache read<span class="cache-num" id="cache-read">—</span></div>
          <div class="cache-row"><span class="swatch" style="background:${COLORS.cacheCreate}"></span>Cache write<span class="cache-num" id="cache-write">—</span></div>
          <div class="cache-row"><span class="swatch" style="background:${COLORS.input}"></span>Fresh input<span class="cache-num" id="cache-fresh">—</span></div>
        </div>
      </div>
    </section>

    <section class="current-strip" id="current-strip">
      <span class="cs-empty">No active session</span>
    </section>
  `;
}

function initCharts(): Charts {
  const mk = (id: string) => echarts.init(el(id), undefined, { renderer: "canvas" });
  const c: Charts = {
    timeseries: mk("chart-timeseries"),
    donut: mk("chart-donut"),
    projects: mk("chart-projects"),
  };
  window.addEventListener("resize", () => {
    c.timeseries.resize();
    c.donut.resize();
    c.projects.resize();
  });
  return c;
}

function bucketLabel(iso: string, range: RangeKey): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  if (range === "today") {
    return d.toLocaleTimeString([], { hour: "2-digit" });
  }
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

function finiteNumber(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function finiteNullable(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function normalizeSummary(input: Summary | null | undefined, fallbackRange: RangeKey): Summary {
  const raw = (input ?? {}) as Partial<Summary>;
  const rawTotals = (raw.totals ?? {}) as Partial<Totals>;
  const totals = {
    input: finiteNumber(rawTotals.input),
    output: finiteNumber(rawTotals.output),
    cacheCreate: finiteNumber(rawTotals.cacheCreate),
    cacheRead: finiteNumber(rawTotals.cacheRead),
  };
  const tokens =
    finiteNullable(rawTotals.tokens) ??
    totals.input + totals.output + totals.cacheCreate + totals.cacheRead;
  const denom = totals.input + totals.cacheCreate + totals.cacheRead;

  return {
    range: typeof raw.range === "string" ? raw.range : fallbackRange,
    totals: {
      tokens,
      ...totals,
      costUsd: finiteNullable(rawTotals.costUsd),
      cacheHitRate:
        finiteNullable(rawTotals.cacheHitRate) ?? (denom > 0 ? totals.cacheRead / denom : 0),
      messages: finiteNumber(rawTotals.messages),
      sessions: finiteNumber(rawTotals.sessions),
    },
    byModel: (Array.isArray(raw.byModel) ? raw.byModel : []).map((m) => ({
      model: typeof m?.model === "string" ? m.model : "unknown",
      tokens: finiteNumber(m?.tokens),
      costUsd: finiteNullable(m?.costUsd),
    })),
    byProject: (Array.isArray(raw.byProject) ? raw.byProject : []).map((p) => ({
      project: typeof p?.project === "string" ? p.project : "unknown",
      tokens: finiteNumber(p?.tokens),
    })),
    bySource: (Array.isArray(raw.bySource) ? raw.bySource : []).map((b) => ({
      source: b?.source === "codex" ? "codex" : "claude",
      tokens: finiteNumber(b?.tokens),
      costUsd: finiteNullable(b?.costUsd),
    })) as BySource[],
    timeseries: (Array.isArray(raw.timeseries) ? raw.timeseries : []).map((b) => ({
      bucket: typeof b?.bucket === "string" ? b.bucket : "",
      input: finiteNumber(b?.input),
      output: finiteNumber(b?.output),
      cacheCreate: finiteNumber(b?.cacheCreate),
      cacheRead: finiteNumber(b?.cacheRead),
    })),
  };
}

/** A vertical gradient from the series color (top, ~0.55 alpha) to transparent. */
function areaGradient(color: string): echarts.graphic.LinearGradient {
  return new echarts.graphic.LinearGradient(0, 0, 0, 1, [
    { offset: 0, color: withAlpha(color, 0.55) },
    { offset: 1, color: withAlpha(color, 0.02) },
  ]);
}

/** Apply an alpha (0..1) to a #rrggbb hex color, returning an rgba() string. */
function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const int = parseInt(m[1], 16);
  const r = (int >> 16) & 0xff;
  const g = (int >> 8) & 0xff;
  const b = int & 0xff;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function renderTimeseries(chart: echarts.ECharts, s: Summary): void {
  const x = s.timeseries.map((b) => bucketLabel(b.bucket, s.range as RangeKey));
  // Single-bucket case: a smooth area with one point renders nothing. Keep
  // boundaryGap false so the area spans the plot edge-to-edge and a lone point
  // still anchors a visible filled column.
  const singlePoint = x.length <= 1;
  const series = (
    [
      ["Fresh input", "input", COLORS.input],
      ["Output", "output", COLORS.output],
      ["Cache write", "cacheCreate", COLORS.cacheCreate],
      ["Cache read", "cacheRead", COLORS.cacheRead],
    ] as const
  ).map(([name, key, color]) => ({
    name,
    type: "line" as const,
    stack: "tok",
    smooth: true,
    showSymbol: singlePoint, // show the marker when there's only one bucket
    symbolSize: 6,
    lineStyle: { width: 1.5, color },
    itemStyle: { color },
    areaStyle: { color: areaGradient(color), opacity: 1 },
    emphasis: { focus: "series" as const },
    data: s.timeseries.map((b) => b[key]),
  }));

  chart.setOption(
    {
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "line", lineStyle: { color: COLORS.axis } },
        valueFormatter: (v: number) => formatTokens(v),
      },
      legend: {
        data: series.map((x) => x.name),
        textStyle: { color: COLORS.label },
        top: 0,
      },
      grid: { left: 56, right: 16, top: 36, bottom: 28 },
      xAxis: {
        type: "category",
        boundaryGap: false,
        data: x,
        axisLine: { lineStyle: { color: COLORS.axis } },
        axisLabel: { color: COLORS.label },
      },
      yAxis: {
        type: "value",
        axisLabel: { color: COLORS.label, formatter: (v: number) => formatTokens(v) },
        splitLine: { lineStyle: { color: COLORS.axis } },
      },
      series,
    },
    { notMerge: true },
  );
}

function renderDonut(chart: echarts.ECharts, s: Summary): void {
  const palette = ["#da7757", "#7c93c3", "#5bc0a8", "#e0a96d", "#b07cc3"];
  chart.setOption({
    backgroundColor: "transparent",
    tooltip: {
      trigger: "item",
      formatter: (p: any) => `${p.name}<br/>${formatTokens(p.value)} (${p.percent}%)`,
    },
    legend: { bottom: 0, textStyle: { color: COLORS.label } },
    series: [
      {
        type: "pie",
        radius: ["46%", "70%"],
        center: ["50%", "44%"],
        avoidLabelOverlap: true,
        itemStyle: { borderColor: "#1a1a1a", borderWidth: 2 },
        label: { show: false },
        data: s.byModel.map((m, i) => ({
          name: m.model.replace("claude-", ""),
          value: m.tokens,
          itemStyle: { color: palette[i % palette.length] },
        })),
      },
    ],
  });
}

function renderProjects(chart: echarts.ECharts, s: Summary): void {
  const sorted = [...s.byProject].sort((a, b) => a.tokens - b.tokens).slice(-8);
  chart.setOption({
    backgroundColor: "transparent",
    tooltip: {
      trigger: "axis",
      axisPointer: { type: "shadow" },
      valueFormatter: (v: number) => formatTokens(v),
    },
    grid: { left: 8, right: 48, top: 8, bottom: 8, containLabel: true },
    xAxis: {
      type: "value",
      axisLabel: { color: COLORS.label, formatter: (v: number) => formatTokens(v) },
      splitLine: { lineStyle: { color: COLORS.axis } },
    },
    yAxis: {
      type: "category",
      data: sorted.map((p) => p.project),
      axisLabel: { color: COLORS.label },
      axisLine: { lineStyle: { color: COLORS.axis } },
    },
    series: [
      {
        type: "bar",
        data: sorted.map((p) => p.tokens),
        itemStyle: { color: "#da7757", borderRadius: [0, 4, 4, 0] },
        barWidth: "60%",
      },
    ],
  });
}

// Last numeric value rendered into each tweened element, so updates animate
// from the previous value (not from 0). The very first tween counts up from 0.
const lastValues = new Map<string, number>();

/** Tween a KPI element from its remembered value to `to`, formatting each frame. */
function tweenKpi(id: string, to: number, format: (v: number) => string): void {
  const node = el(id);
  const from = lastValues.get(id) ?? 0;
  lastValues.set(id, to);
  animateNumber(node, from, to, { format });
}

function renderKpis(s: Summary): void {
  const t = s.totals;
  tweenKpi("kpi-tokens", t.tokens, formatTokens);
  el("kpi-tokens-sub").textContent = `${formatTokens(t.input)} in · ${formatTokens(t.output)} out`;
  retweenCost(t.costUsd);
  tweenKpi("kpi-cache", t.cacheHitRate, formatPct);
  el("kpi-cache-sub").textContent = `${formatTokens(t.cacheRead)} cached reads`;
  renderSessionsKpi(t.sessions, t.messages);
  el("kpi-sessions-sub").textContent = "sessions / messages";
}

/** Tween the cost value, formatting the live value as currency each frame. */
function retweenCost(costUsd: number | null): void {
  const node = el("kpi-cost");
  if (costUsd === null) {
    lastValues.delete("kpi-cost");
    node.textContent = "—";
    return;
  }
  const from = lastValues.get("kpi-cost") ?? 0;
  lastValues.set("kpi-cost", costUsd);
  animateNumber(node, from, costUsd, { format: (v) => formatCost(v) });
}

/** Tween the combined "sessions / messages" line; both numbers animate. */
function renderSessionsKpi(sessions: number, messages: number): void {
  const node = el("kpi-sessions");
  const fromS = lastValues.get("kpi-sessions") ?? 0;
  const fromM = lastValues.get("kpi-messages") ?? 0;
  lastValues.set("kpi-sessions", sessions);
  lastValues.set("kpi-messages", messages);
  const span = messages - fromM;
  // Drive the line off the sessions tween, deriving messages proportionally so
  // both numbers settle together on a single rAF loop.
  animateNumber(node, fromS, sessions, {
    format: (v) => {
      const denom = sessions - fromS;
      const progress = denom !== 0 ? (v - fromS) / denom : 1;
      const msgs = fromM + span * progress;
      return `${formatInt(v)} / ${formatInt(msgs)}`;
    },
  });
}

function renderCachePanel(s: Summary): void {
  const t = s.totals;
  tweenKpi("cache-big", t.cacheHitRate, formatPct);
  el("cache-read").textContent = formatTokens(t.cacheRead);
  el("cache-write").textContent = formatTokens(t.cacheCreate);
  el("cache-fresh").textContent = formatTokens(t.input);
}

// Last token value rendered into each session card's count, keyed by session id,
// so live updates tween from the prior value instead of snapping or restarting.
const lastSessionTokens = new Map<string, number>();

function stateLabelFor(state: SessionState["state"]): string {
  return state.kind === "working" && state.tool
    ? `working · ${state.tool}`
    : state.kind;
}

function sessionRowMarkup(session: SessionState): string {
  return `
    <div class="cs-row" data-session="${escapeHtml(session.sessionId)}">
      <span class="cs-dot cs-${session.state.kind}"></span>
      <span class="cs-project">${escapeHtml(session.project)}</span>
      <span class="cs-sep">·</span>
      <span class="cs-model">${escapeHtml(session.model.replace("claude-", ""))}</span>
      <span class="cs-sep">·</span>
      <span class="cs-tokens" data-session="${escapeHtml(session.sessionId)}">${formatTokens(session.tokens)} tok</span>
      <span class="cs-sep">·</span>
      <span class="cs-state">${escapeHtml(stateLabelFor(session.state))}</span>
    </div>`;
}

/**
 * Render ONE strip-row per active session (wrapped, no scrollbar). Empty state
 * is preserved when there are none. Token counts tween from their prior value.
 */
function renderSessions(sessions: SessionState[]): void {
  const strip = el("current-strip");
  if (!sessions.length) {
    lastSessionTokens.clear();
    strip.innerHTML = `<span class="cs-empty">No active session</span>`;
    return;
  }

  strip.innerHTML = sessions.map(sessionRowMarkup).join("");

  // Tween each card's token count from its remembered value (count up from 0
  // on first appearance), and drop ids that are no longer active.
  const live = new Set(sessions.map((s) => s.sessionId));
  for (const id of [...lastSessionTokens.keys()]) {
    if (!live.has(id)) lastSessionTokens.delete(id);
  }
  for (const s of sessions) {
    const node = strip.querySelector<HTMLElement>(
      `.cs-tokens[data-session="${cssAttrEscape(s.sessionId)}"]`,
    );
    if (!node) continue;
    const from = lastSessionTokens.get(s.sessionId) ?? 0;
    lastSessionTokens.set(s.sessionId, s.tokens);
    animateNumber(node, from, s.tokens, { format: (v) => `${formatTokens(v)} tok` });
  }
}

/** Escape a session id for use inside a CSS attribute selector. */
function cssAttrEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}

/**
 * Back-compat single-session entry point: renders the given session (or the
 * empty state) by delegating to the multi-session renderer.
 */
function renderCurrent(session: SessionState | null): void {
  renderSessions(session ? [session] : []);
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

function setActiveTab(range: RangeKey): void {
  const tabs = el("range-tabs").querySelectorAll<HTMLButtonElement>(".range-tab");
  tabs.forEach((tab) => {
    tab.classList.toggle("active", tab.dataset.range === range);
  });
}

/**
 * Update the NUMERIC readouts only (KPI tweens, cache panel, by-source split,
 * token-ticker odometers). Cheap enough to run on the 2Hz heartbeat — does NOT
 * touch the ECharts charts.
 */
function updateNumbers(summary: Summary, tickerMode: "roll" | "snap" = "roll"): void {
  renderKpis(summary);
  renderCachePanel(summary);
  renderBySource(el("bysource"), summary.bySource as BySource[]);
  updateTokenTicker(el("token-ticker"), summary, tickerMode);
}

/** Re-render the ECharts charts. Kept off the 2Hz loop (would thrash). */
function updateCharts(summary: Summary): void {
  if (!charts) return;
  renderTimeseries(charts.timeseries, summary);
  renderDonut(charts.donut, summary);
  renderProjects(charts.projects, summary);
}

function renderSummary(summary: Summary, tickerMode: "roll" | "snap" = "roll"): void {
  updateNumbers(summary, tickerMode);
  updateCharts(summary);
}

async function loadRange(range: RangeKey): Promise<void> {
  currentRange = range;
  setActiveTab(range);
  const summary = normalizeSummary(await getSummary(range), range);
  // Range switch: SNAP the odometers so we don't roll for seconds across
  // wildly different totals (e.g. today -> all).
  renderSummary(summary, "snap");
}

/** Re-fetch the current range's summary and re-render KPIs + charts in place. */
async function refreshSummary(): Promise<void> {
  const summary = normalizeSummary(await getSummary(currentRange), currentRange);
  renderSummary(summary, "roll");
}

// --- 2Hz numeric heartbeat -------------------------------------------------
// The user wants the readouts to BEAT in real time at a steady 2Hz, with the
// odometers rolling. This loop re-fetches the summary (+ sessions) every 500ms
// and updates ONLY the numeric readouts — the token-ticker odometers, the KPI
// numbers, and the session-strip token counts. Charts are deliberately left to
// the SSE/event path; re-rendering ECharts at 2Hz would thrash.
//
// Data-granularity reality: the underlying token totals only change when Claude
// finishes a message (per-message granularity). 2Hz is purely the DISPLAY
// cadence — between messages the numbers are identical and the odometer simply
// holds (unchanged columns don't roll).
const HEARTBEAT_MS = 500;
let heartbeatTimer: ReturnType<typeof setInterval> | null = null;
let heartbeatInFlight = false;

/** One heartbeat tick: refresh numeric readouts only. Skipped while hidden. */
async function heartbeatTick(): Promise<void> {
  // Tray-first app: usually hidden. Don't hammer the DB/HTTP while hidden.
  if (typeof document !== "undefined" && document.visibilityState !== "visible") {
    return;
  }
  if (heartbeatInFlight) return; // never let ticks overlap on a slow fetch
  heartbeatInFlight = true;
  try {
    const [summary, sessions] = await Promise.all([
      getSummary(currentRange),
      getSessions(),
    ]);
    // "roll": feed setTarget so the odometers perpetually ease UP toward the
    // freshest total (latency does not matter — the readout keeps climbing).
    updateNumbers(normalizeSummary(summary, currentRange), "roll");
    renderSessions(sessions);
  } finally {
    heartbeatInFlight = false;
  }
}

/** Start the steady 2Hz numeric refresh loop (idempotent). */
function startHeartbeat(): void {
  if (typeof setInterval !== "function" || heartbeatTimer !== null) return;
  heartbeatTimer = setInterval(() => void heartbeatTick(), HEARTBEAT_MS);
  // Fetch immediately when the dashboard becomes visible again so the numbers
  // are fresh the instant the user looks, rather than up to 500ms stale.
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") void heartbeatTick();
    });
  }
}

let lastLimits: Limits | null = null;

/** Fetch limits and render the panel; remembers the snapshot for countdown ticks. */
async function refreshLimits(): Promise<void> {
  const limits = await getLimits();
  lastLimits = limits;
  renderLimits(el("limits-body"), limits);
}

// Debounce live summary refreshes so a burst of SSE frames coalesces into one
// fetch. Kept tiny (~next tick) so the KPI row + token ticker update the
// instant new token data lands — just enough to coalesce a burst, not enough
// to feel laggy. Data-granularity reality: Claude records token usage once per
// completed assistant message, so the totals STEP per-message; the tween then
// glides between those steps to feel continuous.
const SUMMARY_DEBOUNCE_MS = 70;
let summaryDebounce: ReturnType<typeof setTimeout> | null = null;

function scheduleSummaryRefresh(): void {
  if (summaryDebounce !== null) clearTimeout(summaryDebounce);
  summaryDebounce = setTimeout(() => {
    summaryDebounce = null;
    void refreshSummary();
    void refreshLimits();
  }, SUMMARY_DEBOUNCE_MS);
}

function handleEvent(ev: CmServerEvent): void {
  if (ev.type === "sessions") {
    // Authoritative live list: one card per active session.
    renderSessions(ev.data);
  } else if (ev.type === "usage") {
    // A single session changed — refresh the KPI row + token ticker (which the
    // summary feeds). The strip itself is driven by the `sessions` event above.
    scheduleSummaryRefresh();
  } else if (ev.type === "import") {
    const bar = el("import-bar");
    const { done, total } = ev.data;
    if (total > 0 && done < total) {
      bar.hidden = false;
      const pct = Math.round((done / total) * 100);
      (el("import-fill") as HTMLElement).style.width = `${pct}%`;
      el("import-text").textContent = `Importing history… ${formatInt(done)} / ${formatInt(total)}`;
    } else {
      bar.hidden = true;
    }
    scheduleSummaryRefresh();
  }
}

async function bootstrap(): Promise<void> {
  const root = el("app");
  renderShell(root);
  charts = initCharts();
  renderTokenTicker(el("token-ticker"), null);

  el("range-tabs").addEventListener("click", (e) => {
    const target = (e.target as HTMLElement).closest<HTMLButtonElement>(".range-tab");
    if (!target?.dataset.range) return;
    void loadRange(target.dataset.range as RangeKey);
  });

  // Settings gear -> the consolidated settings panel (pets / monitor / sound /
  // volume / Discord). Mounted lazily into its own host so it overlays cleanly.
  const settingsPanel = createSettingsPanel(el("settings-mount"), {
    getSettings,
    updateSettings,
  });
  el("settings-gear").addEventListener("click", () => void settingsPanel.open());

  await loadRange(currentRange);
  renderSessions(await getSessions());
  await refreshLimits();

  // Tick the reset countdowns once per second without re-rendering markup.
  if (typeof setInterval === "function") {
    setInterval(() => {
      if (lastLimits) tickCountdowns(el("limits-body"));
    }, 1_000);
  }

  subscribe(handleEvent);

  // Steady 2Hz display heartbeat (visibility-gated) on top of the event stream.
  startHeartbeat();
}

function autostart(): void {
  // Only auto-boot a real, empty dashboard mount. Lets tests import this
  // module and drive the exported functions without side effects.
  const mount = typeof document !== "undefined" ? document.getElementById("app") : null;
  if (mount && mount.childElementCount === 0) void bootstrap();
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    window.addEventListener("DOMContentLoaded", autostart);
  } else {
    autostart();
  }
}

export { loadRange, handleEvent, renderCurrent, renderSessions, bootstrap, normalizeSummary };
