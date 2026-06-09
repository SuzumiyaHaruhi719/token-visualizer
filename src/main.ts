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
  getFx,
} from "./lib/api";
import { createSettingsPanel } from "./components/settings-panel";
import { formatTokens, formatPct, formatInt } from "./lib/format";
import {
  formatCost as formatCurrency,
  asCurrency,
  type CurrencyCode,
  type FxRates,
} from "./lib/currency";
import { animateNumber } from "./lib/tween";
import {
  animateHeightChange,
  revealOnNextFrame,
  revealContent,
  formatFreshness,
  sourceIconSvg,
} from "./lib/motion";
import { renderLimits, tickCountdowns } from "./components/limits";
import { renderBySource } from "./components/by-source";
import { renderTokenTicker, updateTokenTicker } from "./components/token-ticker";
import { LiveCreep } from "./lib/live-creep";
import type {
  Summary,
  SessionState,
  RangeKey,
  CmServerEvent,
  Totals,
  BySource,
  Limits,
  Source,
} from "./lib/types";

/** The known source ids; anything else (or absent) normalizes to "claude". */
const KNOWN_SOURCES: readonly Source[] = ["claude", "codex", "deepseek"];

/** Narrow an untrusted source value from the API to a known {@link Source}.
 *  Absent / unknown -> "claude" (the backend defaults Claude sessions). */
function normalizeSource(value: unknown): Source {
  return KNOWN_SOURCES.includes(value as Source) ? (value as Source) : "claude";
}

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

const CHART_MOTION = {
  animation: true,
  animationDuration: 700,
  animationDurationUpdate: 520,
  animationEasing: "cubicInOut",
  animationEasingUpdate: "cubicInOut",
} as const;

const CHART_STILL = {
  animation: false,
} as const;

type ChartMotion = typeof CHART_MOTION | typeof CHART_STILL;

type DonutTooltipParam = {
  name: string;
  value: number | string;
  percent: number;
};

type Charts = {
  timeseries: echarts.ECharts;
  donut: echarts.ECharts;
  projects: echarts.ECharts;
};

let currentRange: RangeKey = "today";
// The range whose data is actually RENDERED right now. Distinct from
// `currentRange` (the selected/requested range): it only advances when a load
// truly applies its summary. Used so the cross-fade fires for the winning
// render even after a rapid double-click, and so background refreshes can tell
// a range swap is still pending.
let charts: Charts | null = null;
// Monotonic token for tab-switch fetches; only the latest one applies its
// result (guards against out-of-order getSummary resolution on fast clicks).
let loadSeq = 0;

// --- billing currency -------------------------------------------------------
// All backend cost figures are USD; we convert on display. The chosen currency
// comes from /api/settings and the USD-based rates from /api/fx (cached daily).
// Both are refreshed on load and whenever the settings panel changes them.
let currentCurrency: CurrencyCode = "USD";
let currentRates: FxRates = {};

/** Currency-aware cost format using the live currency + rates. */
function fmtCost(usd: number | null | undefined): string {
  return formatCurrency(usd, currentCurrency, currentRates);
}

/** Refresh the chosen currency (from settings) + USD-based rates (from /api/fx),
 *  then repaint the cost readouts so a currency change applies immediately. */
async function refreshCurrency(): Promise<void> {
  const [settings, fx] = await Promise.all([getSettings(), getFx()]);
  currentCurrency = asCurrency(settings.currency);
  currentRates = fx.rates ?? {};
  // Re-render the KPI cost in place (no tween) so a currency switch is instant.
  const node = document.getElementById("kpi-cost");
  if (node && lastValues.has("kpi-cost")) {
    node.textContent = fmtCost(lastValues.get("kpi-cost")!);
  }
}

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
        <span class="range-indicator" id="range-indicator"></span>
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
      <div class="panel-head">
        <h2 class="panel-title">Session limits</h2>
        <button class="limits-refresh" id="limits-refresh" type="button"
                aria-label="Refresh limits" title="Refresh">⟳</button>
      </div>
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
    // The backend already bakes the user's LOCAL wall-clock hour into the bucket
    // string but tags it with a misleading "Z" (a 01:00-local event becomes
    // "...T01:00:00Z"). Render that baked hour AS-IS with timeZone:"UTC" so we
    // don't re-apply the local offset a second time (which turned 01 into 09 at
    // UTC+8). See crates/core/src/query.rs `query_timeseries`.
    return d.toLocaleTimeString([], { hour: "2-digit", timeZone: "UTC" });
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
      source: normalizeSource(b?.source),
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

/** Read the legend's current on/off selection so it survives a re-render. */
function legendSelection(chart: echarts.ECharts): Record<string, boolean> | undefined {
  if (typeof chart.getOption !== "function") return undefined; // e.g. test mock
  const opt = chart.getOption() as { legend?: Array<{ selected?: Record<string, boolean> }> };
  return opt?.legend?.[0]?.selected;
}

function renderTimeseries(
  chart: echarts.ECharts,
  s: Summary,
  chartMotion: ChartMotion = CHART_MOTION,
): void {
  // "today" uses an hourly intraday curve drawn as a smooth gradient AREA chart;
  // every other range uses daily buckets drawn as stacked BARS.
  const isDay = (s.range as RangeKey) === "today";
  const x = s.timeseries.map((b) => bucketLabel(b.bucket, s.range as RangeKey));
  const singlePoint = x.length <= 1;
  const defs = [
    ["Fresh input", "input", COLORS.input],
    ["Output", "output", COLORS.output],
    ["Cache write", "cacheCreate", COLORS.cacheCreate],
    ["Cache read", "cacheRead", COLORS.cacheRead],
  ] as const;

  const series = defs.map(([name, key, color]) =>
    isDay
      ? {
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
        }
      : {
          name,
          type: "bar" as const,
          stack: "tok",
          itemStyle: { color },
          barMaxWidth: 36,
          emphasis: { focus: "series" as const },
          data: s.timeseries.map((b) => b[key]),
        },
  );

  // Preserve the user's legend toggles across re-renders. Without this, every
  // live refresh (which uses notMerge to swap chart type cleanly) would reset
  // the selection — so clicking "Cache read" appeared to bounce right back.
  const selected = legendSelection(chart);

  chart.setOption(
    {
      ...chartMotion,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        axisPointer: {
          type: isDay ? "line" : "shadow",
          lineStyle: { color: COLORS.axis },
        },
        valueFormatter: (v: number) => formatTokens(v),
      },
      legend: {
        data: defs.map((d) => d[0]),
        textStyle: { color: COLORS.label },
        top: 0,
        ...(selected ? { selected } : {}),
      },
      grid: { left: 56, right: 16, top: 36, bottom: 28 },
      xAxis: {
        type: "category",
        boundaryGap: !isDay, // bars sit inside categories; the area spans edge-to-edge
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

function renderDonut(
  chart: echarts.ECharts,
  s: Summary,
  chartMotion: ChartMotion = CHART_MOTION,
): void {
  const palette = ["#da7757", "#7c93c3", "#5bc0a8", "#e0a96d", "#b07cc3"];
  chart.setOption({
    ...chartMotion,
    backgroundColor: "transparent",
    tooltip: {
      trigger: "item",
      formatter: (p: DonutTooltipParam) => {
        const value = typeof p.value === "number" ? p.value : Number(p.value);
        return `${p.name}<br/>${formatTokens(value)} (${p.percent}%)`;
      },
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

function renderProjects(
  chart: echarts.ECharts,
  s: Summary,
  chartMotion: ChartMotion = CHART_MOTION,
): void {
  const sorted = [...s.byProject].sort((a, b) => a.tokens - b.tokens).slice(-8);
  chart.setOption({
    ...chartMotion,
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
  animateNumber(node, from, costUsd, { format: (v) => fmtCost(v) });
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

// Human-friendly label per pet-state kind. The backend now distinguishes
// reasoning (thinking) from tool runs (working) and text output (responding),
// so the strip reflects what each session is actually doing instead of
// collapsing everything to "responding".
const STATE_LABELS: Record<SessionState["state"]["kind"], string> = {
  thinking: "thinking",
  working: "working",
  responding: "responding",
  waiting: "waiting",
  idle: "idle",
  sleeping: "sleeping",
};

function stateLabelFor(state: SessionState["state"]): string {
  if (state.kind === "working") {
    return state.tool ? `working · ${state.tool}` : "working";
  }
  return STATE_LABELS[state.kind];
}

/** Active states drive a subtle pulse on the strip dot (reasoning counts). */
const ACTIVE_KINDS: ReadonlySet<SessionState["state"]["kind"]> = new Set([
  "thinking",
  "working",
  "responding",
]);

function isActiveState(state: SessionState["state"]): boolean {
  return ACTIVE_KINDS.has(state.kind);
}

/** A session's source; absent / unknown means Claude (older payloads omit the
 *  field and the backend defaults Claude sessions). Used to pick the live-strip
 *  source glyph + accent (Claude / Codex / DeepSeek). */
function sessionSource(s: SessionState): Source {
  return normalizeSource(s.source);
}

/** The optional last-user-message, trimmed; "" when absent/blank. */
function lastUserMessageOf(s: SessionState): string {
  return typeof s.lastUserMessage === "string" ? s.lastUserMessage.trim() : "";
}

/** The last user message for a session row's primary text. Falls back to a
 *  muted placeholder when the backend hasn't captured one yet (older payloads /
 *  a session that has only system turns so far). */
function sessionMessage(s: SessionState): string {
  const msg = lastUserMessageOf(s);
  return msg.length > 0 ? msg : "—";
}

function sessionRowMarkup(session: SessionState, entering: boolean): string {
  const dotActive = isActiveState(session.state) ? " cs-active" : "";
  const source = sessionSource(session);
  const hasMsg = lastUserMessageOf(session).length > 0;
  const id = escapeHtml(session.sessionId);
  // Primary text is the LAST USER MESSAGE (project name removed, per spec). The
  // source icon precedes it; model + tokens are dimmed secondary; the state pill
  // and a live freshness label trail. Freshness is re-rendered on the heartbeat
  // via the data-updated attr (see refreshFreshness).
  return `
    <div class="cs-row${entering ? " ui-enter" : ""}" data-session="${id}">
      ${sourceIconSvg(source, "cs-source-icon")}
      <span class="cs-msg${hasMsg ? "" : " cs-msg-empty"}" title="${escapeHtml(sessionMessage(session))}">${escapeHtml(sessionMessage(session))}</span>
      <span class="cs-model">${escapeHtml(session.model.replace("claude-", ""))}</span>
      <span class="cs-tokens" data-session="${id}">${formatTokens(session.tokens)} tok</span>
      <span class="cs-state cs-state-${session.state.kind}"><span class="cs-dot cs-${session.state.kind}${dotActive}"></span>${escapeHtml(stateLabelFor(session.state))}</span>
      <span class="cs-freshness" data-updated="${Number.isFinite(session.updatedAt) ? session.updatedAt : ""}">${escapeHtml(formatFreshness(session.updatedAt))}</span>
    </div>`;
}

/** Re-render every visible row's freshness label from its `data-updated` epoch
 *  stamp. Cheap text-only update run on the heartbeat tick so "Ns ago" advances
 *  live without rebuilding the rows (which would restart token tweens). */
function refreshFreshness(nowMs: number = Date.now()): void {
  const strip = document.getElementById("current-strip");
  if (!strip) return;
  strip.querySelectorAll<HTMLElement>(".cs-freshness[data-updated]").forEach((node) => {
    const raw = node.getAttribute("data-updated");
    if (!raw) return;
    node.textContent = formatFreshness(Number(raw), nowMs);
  });
}

/**
 * Render ONE strip-row per active session (wrapped, no scrollbar). Empty state
 * is preserved when there are none. Token counts tween from their prior value.
 */
function renderSessions(sessions: SessionState[]): void {
  const strip = el("current-strip");
  const previousIds = new Set(lastSessionTokens.keys());
  // Smoothly transition the strip's height when rows are added/removed (FLIP);
  // a no-op when the height is unchanged (the common per-tick re-render).
  animateHeightChange(strip, () => {
    if (!sessions.length) {
      lastSessionTokens.clear();
      strip.innerHTML = `<span class="cs-empty">No active session</span>`;
      return;
    }

    strip.innerHTML = sessions
      .map((session) => sessionRowMarkup(session, !previousIds.has(session.sessionId)))
      .join("");

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
    strip.querySelectorAll(".cs-row.ui-enter").forEach((row) => {
      revealOnNextFrame(row, "ui-enter");
    });
  });
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
  positionRangeIndicator(range);
}

/**
 * Slide the active-tab pill under the selected range tab. The indicator is one
 * absolutely-positioned element that transitions its transform + size (CSS), so
 * switching tabs glides the pill instead of hard-cutting the highlight.
 */
function positionRangeIndicator(range: RangeKey): void {
  const indicator = document.getElementById("range-indicator");
  const btn = el("range-tabs").querySelector<HTMLElement>(
    `.range-tab[data-range="${range}"]`,
  );
  if (!indicator || !btn) return;
  indicator.style.width = `${btn.offsetWidth}px`;
  indicator.style.height = `${btn.offsetHeight}px`;
  indicator.style.transform = `translate(${btn.offsetLeft}px, ${btn.offsetTop}px)`;
}

/**
 * Update the NUMERIC readouts only (KPI tweens, cache panel, by-source split,
 * token-ticker odometers). Cheap enough to run on the 2Hz heartbeat — does NOT
 * touch the ECharts charts.
 */
type TickerMode = "roll" | "snap" | "transition";

function updateNumbers(
  summary: Summary,
  tickerMode: TickerMode = "roll",
  totalOverride?: number,
): void {
  renderKpis(summary);
  renderCachePanel(summary);
  renderBySource(el("bysource"), summary.bySource as BySource[]);
  updateTokenTicker(el("token-ticker"), summary, tickerMode, totalOverride);
}

/** Re-render the ECharts charts. Kept off the 2Hz loop (would thrash). */
function updateCharts(summary: Summary, chartMotion: ChartMotion = CHART_MOTION): void {
  if (!charts) return;
  renderTimeseries(charts.timeseries, summary, chartMotion);
  renderDonut(charts.donut, summary, chartMotion);
  renderProjects(charts.projects, summary, chartMotion);
}

function renderSummary(
  summary: Summary,
  tickerMode: TickerMode = "roll",
  totalOverride?: number,
  chartMotion: ChartMotion = CHART_MOTION,
): void {
  updateNumbers(summary, tickerMode, totalOverride);
  updateCharts(summary, chartMotion);
}

/**
 * The major dashboard blocks that cross-fade on a range switch: the KPI
 * cluster, the by-source split, and each ECharts panel + the limits panel
 * (faded by their `.panel` wrapper, so title + canvas move as one object and
 * ECharts sizing is never touched). Re-queried each call since `renderSummary`
 * may replace some internals.
 */
function blockSwapTargets(): (Element | null)[] {
  return [
    document.querySelector(".kpis"),
    document.getElementById("bysource"),
    document.getElementById("chart-timeseries")?.closest(".panel") ?? null,
    document.getElementById("chart-donut")?.closest(".panel") ?? null,
    document.getElementById("chart-projects")?.closest(".panel") ?? null,
    document.getElementById("limits-body")?.closest(".panel") ?? null,
  ];
}

async function loadRange(range: RangeKey): Promise<void> {
  // Sequence guard: fast tab clicks fire overlapping loadRange calls whose
  // getSummary fetches can resolve OUT OF ORDER. Without this, a stale (earlier)
  // range's result could land last and win — and worse, set `rollFloor` to that
  // range's total, which the raise-only heartbeat then locks in (the tab says
  // "today" but the number stays stuck on 30d). Only the most-recent click
  // applies its result.
  const seq = ++loadSeq;
  currentRange = range;
  setActiveTab(range);

  // No panel cross-fade: dipping every block to opacity 0 read as a flicker.
  // Switching the time range should only TRANSITION the numbers (a quick roll)
  // and update the visualizations in place — never blank the panels. Out-of-order
  // fetches are dropped by the `loadSeq` guard below; background refreshes
  // (heartbeat / SSE) self-drop via their own range-token check, so there's no
  // `rangeSwapPending` flag to get stuck `true` if a fetch ever stalls.
  const summary = normalizeSummary(await getSummary(range), range);
  if (seq !== loadSeq) return; // superseded by a newer tab switch — drop this stale result

  // In-place switch: the headline total TRANSITIONS (a quick fixed-duration roll)
  // to the new range's total; the KPI numbers tween, the by-source split
  // transitions its segment widths from their current values, and the charts
  // repaint. No opacity fade.
  renderSummary(summary, "transition", undefined, CHART_STILL);
  // Range switched: restart the live creep at the new range's real total so it
  // doesn't carry the previous range's (much larger/smaller) projection.
  liveCreep.reset(summary.totals.tokens, Date.now());
  creepWasActive = anySessionActive;
}

// --- live "creep" for the headline total -----------------------------------
// The big "Total tokens · live" reel must keep rolling while ANY session is
// active, but real totals only step per completed message. `liveCreep` projects
// a continuously-advancing total in those gaps (see lib/live-creep.ts) and we
// feed it through the ticker's `totalOverride`; the per-model rows keep their
// EXACT real values. When no session is active it reconciles to the real total
// and stops, so the rAF frees the event loop (preserves the old freeze fix).
const liveCreep = new LiveCreep();
let anySessionActive = false;
let creepWasActive = false;

/** Update the active-session flag from a freshly-known session list. */
function noteSessions(sessions: SessionState[]): void {
  anySessionActive = sessions.some((s) => isActiveState(s.state));
}

/** Feed the creep the latest real total + activity; return the ticker mode +
 *  optional override so the big total rolls continuously while active and eases
 *  back down to the exact total the moment everything goes idle. */
function liveTotalUpdate(summary: Summary): { mode: TickerMode; override?: number } {
  const now = Date.now();
  liveCreep.observeReal(summary.totals.tokens, now);
  liveCreep.setActive(anySessionActive);
  const override = liveCreep.tick(now);
  if (creepWasActive && !anySessionActive) {
    creepWasActive = false;
    return { mode: "transition" }; // active -> idle: ease DOWN to the real total
  }
  creepWasActive = anySessionActive;
  return anySessionActive ? { mode: "roll", override } : { mode: "roll" };
}

/** Re-fetch the current range's summary and re-render KPIs + charts in place. */
async function refreshSummary(): Promise<void> {
  // Capture the range we're fetching for; if a tab switch changes it while the
  // fetch is in flight, drop the result — applying old-range data would retarget
  // the raise-only live creep to a stale total.
  const r = currentRange;
  const summary = normalizeSummary(await getSummary(r), r);
  if (r !== currentRange) return;
  const live = liveTotalUpdate(summary);
  renderSummary(summary, live.mode, live.override);
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
// Heartbeat cadence. A modest 1.5s safety refresh on top of the SSE stream —
// NOT a per-frame perpetual animation. (An earlier build kept a 2Hz creep that
// fed the odometer an ever-rising target so its rAF ran FOREVER; combined with
// the glow/blur/mask compositing that saturated WebView2 and froze the whole
// dashboard. The number now rolls silkily toward the REAL total on each update
// and CONVERGES + stops, freeing the event loop, so the UI stays responsive.)
const HEARTBEAT_MS = 1500;
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
    const r = currentRange;
    const [summary, sessions] = await Promise.all([
      getSummary(r),
      getSessions(),
    ]);
    noteSessions(sessions); // refresh the active flag BEFORE the creep reads it
    // Keep the headline total rolling continuously while any session is active
    // (live creep); per-model rows still track their exact real values. Drop the
    // range-dependent numeric repaint if the range changed during the fetch (that
    // data is for the OLD range; applying it would retarget the raise-only creep
    // to a stale total). The session strip is range-independent, so it always
    // refreshes.
    if (r === currentRange) {
      const norm = normalizeSummary(summary, r);
      const live = liveTotalUpdate(norm);
      updateNumbers(norm, live.mode, live.override);
    }
    renderSessions(sessions);
    refreshFreshness(); // advance each row's "Ns ago" without rebuilding rows
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
// Guard so overlapping refreshes (auto-timer + manual click) don't double-fetch
// or leave the spinner stuck if one resolves while another is mid-flight.
let limitsInFlight = false;
// Auto-refresh the limits panel every N seconds (the countdown interval runs at
// 1Hz, so this is a tick count). ~5s keeps the Codex % fresh without hammering.
const LIMITS_REFRESH_TICKS = 5;

/** Spin the refresh icon while a limits refresh is running (CSS rotation,
 *  honoring prefers-reduced-motion). No-op if the button isn't mounted. */
function setLimitsSpinning(spinning: boolean): void {
  const btn = document.getElementById("limits-refresh");
  if (btn) btn.classList.toggle("spinning", spinning);
}

/** Fetch limits and render the panel; remembers the snapshot for countdown
 *  ticks. Spins the refresh icon for the duration of the fetch (auto + manual).
 *  Overlapping calls are coalesced via `limitsInFlight`. */
async function refreshLimits(): Promise<void> {
  if (limitsInFlight) return;
  limitsInFlight = true;
  setLimitsSpinning(true);
  try {
    const limits = await getLimits();
    lastLimits = limits; // keep the latest snapshot for the 1s countdown ticks
    const body = el("limits-body");
    // Skip the repaint when nothing changed AND the panel is already rendered:
    // refreshLimits runs on every `usage` SSE frame, so re-rendering identical
    // data made the Codex gauges look like they were constantly auto-refreshing.
    // The signature lives on the body itself (not a module var) so a fresh /
    // re-initialized body always repaints.
    const sig = JSON.stringify(limits);
    if (body.dataset.limitsSig === sig && body.childElementCount > 0) return;
    body.dataset.limitsSig = sig;
    animateHeightChange(body, () => renderLimits(body, limits));
  } finally {
    limitsInFlight = false;
    setLimitsSpinning(false);
  }
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
    noteSessions(ev.data); // keep the creep's active gate in sync with live state
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

/** Tauri's injected IPC bridge (present only when running inside the app). */
interface TauriInternals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}
function tauriInternals(): TauriInternals | null {
  const internals = (window as unknown as { __TAURI_INTERNALS__?: TauriInternals })
    .__TAURI_INTERNALS__;
  return internals && typeof internals.invoke === "function" ? internals : null;
}

/** F11 toggles borderless fullscreen on the dashboard window (browser-style).
 *  No-op outside Tauri (plain browser / vitest). Uses the window plugin IPC —
 *  the `value` arg name matches Tauri's `set_fullscreen` command. */
function setupFullscreenToggle(): void {
  const internals = tauriInternals();
  if (!internals) return;
  window.addEventListener("keydown", (e) => {
    if (e.key !== "F11") return;
    e.preventDefault();
    void (async () => {
      try {
        const current = (await internals.invoke("plugin:window|is_fullscreen")) as boolean;
        await internals.invoke("plugin:window|set_fullscreen", { value: !current });
      } catch {
        // best-effort: ignore if denied / unavailable
      }
    })();
  });
}

async function bootstrap(): Promise<void> {
  const root = el("app");
  renderShell(root);
  charts = initCharts();
  renderTokenTicker(el("token-ticker"), null);
  setupFullscreenToggle();

  el("range-tabs").addEventListener("click", (e) => {
    const target = (e.target as HTMLElement).closest<HTMLButtonElement>(".range-tab");
    if (!target?.dataset.range) return;
    void loadRange(target.dataset.range as RangeKey);
  });

  // Settings gear -> the consolidated settings panel (monitor / sound / volume /
  // currency / tray background / Discord). Mounted lazily into its own host so
  // it overlays cleanly. We wrap `updateSettings` so a currency change repaints
  // the cost readouts immediately (the rates are already cached client-side).
  const settingsPanel = createSettingsPanel(el("settings-mount"), {
    getSettings,
    updateSettings: async (patch) => {
      const next = await updateSettings(patch);
      if (patch.currency !== undefined) {
        currentCurrency = asCurrency(next.currency);
        const node = document.getElementById("kpi-cost");
        if (node && lastValues.has("kpi-cost")) {
          node.textContent = fmtCost(lastValues.get("kpi-cost")!);
        }
      }
      return next;
    },
  });
  el("settings-gear").addEventListener("click", () => void settingsPanel.open());

  // Billing currency + USD-based FX rates (cached daily server-side). Fetched
  // before the first paint so costs render in the chosen currency from the off.
  await refreshCurrency();

  await loadRange(currentRange);
  renderSessions(await getSessions());
  await refreshLimits();

  // First-mount entrance: now that every block has content, ease them in (fade
  // + rise) rather than popping. Reduced-motion / test envs no-op cleanly.
  revealContent(blockSwapTargets());

  // Subtle manual refresh: the ⟳ icon in the panel header. Refreshes both the
  // limits panel (Codex %) and the summary (totals), spinning while in flight.
  el("limits-refresh").addEventListener("click", () => {
    void refreshLimits();
    void refreshSummary();
  });

  // Tick the reset countdowns once per second without re-rendering markup, and
  // AUTO-REFRESH the limits panel every LIMITS_REFRESH_TICKS seconds. The live
  // poller only watches ~/.claude, so Codex-only activity fires no SSE and the
  // panel would otherwise freeze at the bootstrap value — this steady cadence
  // keeps the Codex rate-limit % as fresh as the rollout allows. Visibility-
  // gated (like the heartbeat) so we don't poll while the window is hidden.
  if (typeof setInterval === "function") {
    let limitsTick = 0;
    setInterval(() => {
      if (lastLimits) tickCountdowns(el("limits-body"));
      refreshFreshness(); // 1Hz so each session row's "Ns ago" advances live
      const visible =
        typeof document === "undefined" || document.visibilityState === "visible";
      if (visible && ++limitsTick >= LIMITS_REFRESH_TICKS) {
        limitsTick = 0;
        void refreshLimits();
      }
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

export { loadRange, handleEvent, renderCurrent, renderSessions, bootstrap, normalizeSummary, bucketLabel };
