// Dashboard bootstrap: KPIs + ECharts + current-session strip, fed by the
// HTTP API and live SSE stream (with mock fallback).

import * as echarts from "echarts";
import "./styles.css";
import { getSummary, getCurrent, subscribe } from "./lib/api";
import { formatTokens, formatCost, formatPct, formatInt } from "./lib/format";
import type {
  Summary,
  SessionState,
  RangeKey,
  CmServerEvent,
  Totals,
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
      <div class="live" id="live"><span class="dot"></span> live</div>
    </header>

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
    timeseries: (Array.isArray(raw.timeseries) ? raw.timeseries : []).map((b) => ({
      bucket: typeof b?.bucket === "string" ? b.bucket : "",
      input: finiteNumber(b?.input),
      output: finiteNumber(b?.output),
      cacheCreate: finiteNumber(b?.cacheCreate),
      cacheRead: finiteNumber(b?.cacheRead),
    })),
  };
}

function renderTimeseries(chart: echarts.ECharts, s: Summary): void {
  const x = s.timeseries.map((b) => bucketLabel(b.bucket, s.range as RangeKey));
  const series = (
    [
      ["Fresh input", "input", COLORS.input],
      ["Output", "output", COLORS.output],
      ["Cache write", "cacheCreate", COLORS.cacheCreate],
      ["Cache read", "cacheRead", COLORS.cacheRead],
    ] as const
  ).map(([name, key, color]) => ({
    name,
    type: "bar" as const,
    stack: "tok",
    emphasis: { focus: "series" as const },
    itemStyle: { color },
    data: s.timeseries.map((b) => b[key]),
  }));

  chart.setOption({
    backgroundColor: "transparent",
    tooltip: {
      trigger: "axis",
      axisPointer: { type: "shadow" },
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
  });
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

function renderKpis(s: Summary): void {
  const t = s.totals;
  el("kpi-tokens").textContent = formatTokens(t.tokens);
  el("kpi-tokens-sub").textContent = `${formatTokens(t.input)} in · ${formatTokens(t.output)} out`;
  el("kpi-cost").textContent = formatCost(t.costUsd);
  el("kpi-cache").textContent = formatPct(t.cacheHitRate);
  el("kpi-cache-sub").textContent = `${formatTokens(t.cacheRead)} cached reads`;
  el("kpi-sessions").textContent = `${formatInt(t.sessions)} / ${formatInt(t.messages)}`;
  el("kpi-sessions-sub").textContent = "sessions / messages";
}

function renderCachePanel(s: Summary): void {
  const t = s.totals;
  el("cache-big").textContent = formatPct(t.cacheHitRate);
  el("cache-read").textContent = formatTokens(t.cacheRead);
  el("cache-write").textContent = formatTokens(t.cacheCreate);
  el("cache-fresh").textContent = formatTokens(t.input);
}

function renderCurrent(session: SessionState | null): void {
  const strip = el("current-strip");
  if (!session) {
    strip.innerHTML = `<span class="cs-empty">No active session</span>`;
    return;
  }
  const stateLabel =
    session.state.kind === "working" && session.state.tool
      ? `working · ${session.state.tool}`
      : session.state.kind;
  strip.innerHTML = `
    <span class="cs-dot cs-${session.state.kind}"></span>
    <span class="cs-project">${escapeHtml(session.project)}</span>
    <span class="cs-sep">·</span>
    <span class="cs-model">${escapeHtml(session.model.replace("claude-", ""))}</span>
    <span class="cs-sep">·</span>
    <span class="cs-tokens">${formatTokens(session.tokens)} tok</span>
    <span class="cs-sep">·</span>
    <span class="cs-state">${escapeHtml(stateLabel)}</span>
  `;
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

async function loadRange(range: RangeKey): Promise<void> {
  currentRange = range;
  setActiveTab(range);
  const summary = normalizeSummary(await getSummary(range), range);
  renderKpis(summary);
  renderCachePanel(summary);
  if (charts) {
    renderTimeseries(charts.timeseries, summary);
    renderDonut(charts.donut, summary);
    renderProjects(charts.projects, summary);
  }
}

function handleEvent(ev: CmServerEvent): void {
  if (ev.type === "usage") {
    renderCurrent(ev.data.current);
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
  }
}

async function bootstrap(): Promise<void> {
  const root = el("app");
  renderShell(root);
  charts = initCharts();

  el("range-tabs").addEventListener("click", (e) => {
    const target = (e.target as HTMLElement).closest<HTMLButtonElement>(".range-tab");
    if (!target?.dataset.range) return;
    void loadRange(target.dataset.range as RangeKey);
  });

  await loadRange(currentRange);
  const initial = await getCurrent();
  renderCurrent(initial);

  subscribe(handleEvent);
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

export { loadRange, handleEvent, renderCurrent, bootstrap, normalizeSummary };
