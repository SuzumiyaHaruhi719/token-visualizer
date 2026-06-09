// Tray "today" popover bootstrap — a FRAMELESS, RESIZABLE, ACRYLIC card whose
// content adapts to its size.
//
// Sections, revealed top-down as the window grows (height tiers), hidden
// bottom-up as it shrinks (see popover.css `data-tier`):
//   ① Token hero — ALWAYS shown. The SAME slot-machine reel + continuous "creep"
//      the dashboard uses (reused createOdometer({reels:true}) + LiveCreep). The
//      digit precision adapts to WIDTH: narrow → compact ("1.52B" text), wide →
//      full rolling reels ("1,523,456,789").
//   ② Codex session — model + tokens from /api/limits `codex.session` (+ 5h /
//      weekly % when tall). The Claude session is deliberately NOT shown.
//   ③ Top-3 models today — /api/summary?range=today byModel.slice(0,3).
//   ④ Live sessions — the active list from /api/sessions (project · model · state).
//
// Costs anywhere use the Part-1 currency formatter (/api/fx + chosen currency).
//
// Live data: a small heartbeat polls today's summary + sessions + limits + the
// opacity setting; the hero keeps rolling via the same creep semantics as the
// dashboard. We do NOT modify odometer.ts / live-creep.ts — import/reuse only.

import "./popover.css";
import {
  getSummary,
  getSessions,
  getLimits,
  getSettings,
  getFx,
  subscribe,
} from "../lib/api";
import { formatTokens } from "../lib/format";
import { formatFreshness, sourceIconSvg } from "../lib/motion";
import { formatCost, asCurrency, type CurrencyCode, type FxRates } from "../lib/currency";
import { createOdometer, type OdometerHandle } from "../components/odometer";
import { LiveCreep } from "../lib/live-creep";
import type {
  ByModel,
  SessionState,
  Limits,
  CmServerEvent,
  Source,
} from "../lib/types";

/** The known source ids; anything else (or absent) normalizes to "claude". */
const KNOWN_SOURCES: readonly Source[] = ["claude", "codex", "deepseek"];

/** How many models to show in the "top models" list. */
const TOP_N = 3;
/** Rank colors for the top models (coral / teal / blue), matching the dashboard. */
const RANK_COLORS = ["#ff8a5c", "#54d2b4", "#8aa2e0"];
/** Leave-animation duration (must match `.pop-model-row.leaving` in popover.css). */
const LEAVE_MS = 260;
/** Heartbeat cadence (ms): keep the hero rolling + content fresh. Matches the
 *  dashboard's "modest safety refresh on top of SSE" rather than a per-frame loop. */
const HEARTBEAT_MS = 1500;

// Height breakpoints (px of the measured CONTENT box, ~window height minus
// padding) → content tier. Token hero is tier 1 (always shown). Kept in sync
// with POPOVER_H / POPOVER_MIN_H in src-tauri/src/windows.rs (the default size
// lands in tier 3: hero + Codex + top-3 models, no dead space).
const TIER_CODEX_PX = 96; // Codex session + its 5h/weekly limit appears
const TIER_MODELS_PX = 196; // top-3 models appear
const TIER_SESSIONS_PX = 300; // live sessions appear
// Graduated hero precision by WIDTH ("精确到几位数"): the reel ALWAYS rolls, but
// as the popover narrows we show fewer significant digits + a unit suffix so the
// number never overflows. We pick a max DIGIT BUDGET for the reel mantissa from
// the width; the value is shown as the largest scale (K/M/B/T or full) whose
// integer mantissa fits that budget. Wide → full grouped integer (no suffix);
// progressively narrower → `1,479M` → `148M` → `1B`-style compaction. The reel
// reflow (digit count changing) animates natively via the odometer's enter/leave.
const PRECISION_TIERS: { minWidth: number; maxDigits: number }[] = [
  { minWidth: 320, maxDigits: 99 }, // full integer, comma-grouped, no suffix
  { minWidth: 286, maxDigits: 6 }, //  e.g. 147,432K  (the K step — graduated)
  { minWidth: 262, maxDigits: 4 }, //  e.g. 1,479M
  { minWidth: 242, maxDigits: 3 }, //  e.g. 147M
  { minWidth: 0, maxDigits: 2 }, //    e.g. 1B / 15B (tightest)
];

const UNITS: { suffix: string; div: number }[] = [
  { suffix: "T", div: 1_000_000_000_000 },
  { suffix: "B", div: 1_000_000_000 },
  { suffix: "M", div: 1_000_000 },
  { suffix: "K", div: 1_000 },
];

const ACTIVE_KINDS = new Set(["thinking", "working", "responding"]);
function isActiveState(state: SessionState["state"]): boolean {
  return ACTIVE_KINDS.has(state.kind);
}

/** The reused rolling reel for the hero total. Created once, never re-created. */
let heroOdo: OdometerHandle | null = null;
/** The continuous-creep controller (same semantics as the dashboard). */
const liveCreep = new LiveCreep();
let anySessionActive = false;
/** Current reel mantissa digit budget (99 = full integer). Re-evaluated by width. */
let heroMaxDigits = 99;
/** The unit suffix currently shown beside the reel ("" / K / M / B / T). */
let heroSuffix = "";
/** True while the user is dragging the popover AT ALL — either a resize-handle
 *  drag OR a move-drag of the whole card (the data-tauri-drag-region). During a
 *  drag the acrylic window recomposites in DWM every frame; if we ALSO run the
 *  ResizeObserver's layout work (`fitHeroToWidth` forces sync `scrollWidth` +
 *  rebuilds the reel) and keep the odometer's creep rAF painting, the compositor
 *  saturates and the whole display hangs. So while `dragging` is true we do ZERO
 *  heavy work: the RO flush early-returns, the heartbeat/SSE/auto-fit are all
 *  gated, and the reel's rAF is halted (snapTo) on drag start. One `applySize` +
 *  one `refresh` run on drag END so the layout settles exactly once on release
 *  (fix: drag freezes the whole monitor). */
let dragging = false;
/** The last height (logical px) we asked the shell to fit, so we don't spam the
 *  IPC with identical values on every heartbeat. */
let lastFitH = 0;
/** Remembered values so each refresh animates from the last shown figure. */
const lastModelTokens = new Map<string, number>();
/** Last rendered signatures for the sessions list + Codex windows. The heartbeat
 *  (1.5s) and every SSE tick call refresh(); rebuilding the DOM unconditionally
 *  made these flicker + jitter the height every tick. We rebuild ONLY when the
 *  signature changes (the rendered content actually differs). */
let lastSessionsSig = "";
let lastCodexSig = "";
/** Live billing currency + USD-based rates for cost displays. */
let currency: CurrencyCode = "USD";
let rates: FxRates = {};

function el<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`#${id} not found`);
  return node as T;
}

/** Escape a model id for use inside a CSS attribute selector. */
function cssAttrEscape(value: string): string {
  return value.replace(/["\\]/g, "\\$&");
}

/** Drop the noisy `claude-` prefix; leave Codex/other ids as-is. */
function prettyModel(model: string): string {
  return model.replace(/^claude-/, "") || "—";
}

/** Human-friendly per-state label for the live session rows. */
function stateLabel(state: SessionState["state"]): string {
  if (state.kind === "working") return state.tool ? `working · ${state.tool}` : "working";
  return state.kind;
}

/** The agent a live session belongs to; absent / unknown `source` means Claude
 *  (the backend defaults Claude sessions and older payloads omit the field).
 *  Drives the popover row's source glyph + accent (Claude / Codex / DeepSeek). */
function sessionSource(s: SessionState): Source {
  return KNOWN_SOURCES.includes(s.source as Source) ? (s.source as Source) : "claude";
}

/** The optional last-user-message, trimmed; "" when absent/blank. */
function lastUserMessageOf(s: SessionState): string {
  return typeof s.lastUserMessage === "string" ? s.lastUserMessage.trim() : "";
}

/** A session row's primary text: the last user message, or a muted dash. */
function sessionMessage(s: SessionState): string {
  const msg = lastUserMessageOf(s);
  return msg.length > 0 ? msg : "—";
}

/** The static card skeleton (built once; refreshes mutate in place so the reel
 *  rolls + bars transition smoothly). The resize handles sit outside the
 *  pointer-events-none content so frameless edge/corner resize works. */
function skeleton(): string {
  return `
    <div class="pop-content" id="pop-content">
      <div class="pop-today">
        <div class="pop-today-head">
          <span class="pop-eyebrow">Today</span>
          <span class="pop-spend" id="pop-spend">—</span>
        </div>
        <div class="pop-hero" id="pop-hero">
          <span class="pop-hero-num" id="pop-hero-num"></span>
          <span class="pop-hero-suffix" id="pop-hero-suffix" hidden></span>
          <span class="pop-hero-unit">tokens</span>
        </div>
      </div>

      <div class="pop-reveal" data-section="codex">
        <div class="pop-reveal-inner">
          <div class="pop-codex">
            <div class="pop-eyebrow">Codex session</div>
            <div class="pop-codex-head">
              <span class="pop-codex-model" id="pop-codex-model">—</span>
              <span class="pop-codex-tok" id="pop-codex-tok"></span>
            </div>
            <div class="pop-codex-windows" id="pop-codex-windows"></div>
          </div>
        </div>
      </div>

      <div class="pop-reveal" data-section="models">
        <div class="pop-reveal-inner">
          <div class="pop-models">
            <div class="pop-eyebrow">Top models · today</div>
            <ul class="pop-model-list" id="pop-models"></ul>
          </div>
        </div>
      </div>

      <div class="pop-reveal" data-section="sessions">
        <div class="pop-reveal-inner">
          <div class="pop-sessions">
            <div class="pop-eyebrow">Live sessions</div>
            <ul class="pop-sess-list" id="pop-sessions"></ul>
          </div>
        </div>
      </div>
    </div>

    <div class="pop-resize pop-resize-n" data-resize="North"></div>
    <div class="pop-resize pop-resize-s" data-resize="South"></div>
    <div class="pop-resize pop-resize-e" data-resize="East"></div>
    <div class="pop-resize pop-resize-w" data-resize="West"></div>
    <div class="pop-resize pop-resize-ne" data-resize="NorthEast"></div>
    <div class="pop-resize pop-resize-nw" data-resize="NorthWest"></div>
    <div class="pop-resize pop-resize-se" data-resize="SouthEast"></div>
    <div class="pop-resize pop-resize-sw" data-resize="SouthWest"></div>
  `;
}

// --- hero reel (reused odometer + live creep) ------------------------------

/** Resolve the raw token total into the {reel mantissa, suffix} to display for
 *  the current width budget (`heroMaxDigits`). The reel is ALWAYS an integer
 *  (reels can't show a decimal point); we scale by the largest unit whose
 *  mantissa fits the budget. `maxDigits >= 99` → full integer, no suffix. */
function heroParts(value: number): { reel: number; suffix: string } {
  const v = Math.max(0, Math.round(value));
  if (heroMaxDigits >= 99) return { reel: v, suffix: "" };
  if (v < UNITS[UNITS.length - 1].div) return { reel: v, suffix: "" }; // value < 1000

  // Pick the SMALLEST unit (the most significant digits = the most precision)
  // whose integer mantissa is >= 1 and < 10^budget. Walking UNITS in REVERSE
  // (K → M → B → T) means we use K before M before B, so a narrowing window
  // steps full → …K → …M → …B (the K representation appears as the budget
  // shrinks). E.g. 147,432,648 → budget6 "147,432K" → budget3 "147M" → budget2
  // "1B"-style. Without the reverse walk the magnitude unit (M) was chosen first
  // and K never showed.
  const cap = Math.pow(10, heroMaxDigits); // mantissa kept < 10^budget when possible
  let fallback: { suffix: string; mantissa: number } | null = null;
  for (let i = UNITS.length - 1; i >= 0; i--) {
    const u = UNITS[i];
    const mantissa = Math.round(v / u.div);
    if (mantissa < 1) continue; // unit too big — value rounds away to 0
    // The largest unit that still keeps the mantissa >= 1 is the safety net used
    // when NOTHING fits the budget: prefer a slightly-over-budget mantissa
    // (148M → "148M") over the raw integer or a "0B".
    fallback = { suffix: u.suffix, mantissa };
    if (mantissa < cap) return { reel: mantissa, suffix: u.suffix };
  }
  if (fallback) return { reel: fallback.mantissa, suffix: fallback.suffix };
  return { reel: v, suffix: "" };
}

/** Mount/refresh the hero reel (the reused odometer, always rolling). The width
 *  budget controls precision: a unit suffix (K/M/B/T) sits beside a scaled
 *  integer reel, or the full grouped integer when wide. The LiveCreep value is
 *  always RAW tokens; only the DISPLAY mantissa/suffix adapt. A `snap` is used
 *  on first paint and whenever the suffix/scale changes (the mantissa jumps, and
 *  the odometer's setTarget is raise-only so it would otherwise ignore a drop). */
function renderHero(value: number, mode: "snap" | "roll"): void {
  const host = el("pop-hero-num");
  const suffixEl = el("pop-hero-suffix");

  if (!heroOdo) heroOdo = createOdometer({ reels: true });
  if (heroOdo.el.parentElement !== host) {
    host.textContent = "";
    host.appendChild(heroOdo.el);
  }

  const { reel, suffix } = heroParts(value);

  // Suffix change (e.g. M→B, or full↔compact) → the mantissa scale jumps; snap
  // so the reel doesn't try to "roll" across an unrelated scale (and so the
  // raise-only setTarget doesn't ignore a downward mantissa).
  const scaleChanged = suffix !== heroSuffix;
  heroSuffix = suffix;
  if (suffix) {
    suffixEl.textContent = suffix;
    suffixEl.hidden = false;
  } else {
    suffixEl.hidden = true;
    suffixEl.textContent = "";
  }

  if (mode === "snap" || scaleChanged) heroOdo.snapTo(reel);
  else heroOdo.setTarget(reel); // raise-only continuous creep within the same scale
}

/** The local date key ("YYYY-M-D") of the last paint. "today" resets to ~0 at
 *  LOCAL midnight, but the reel/creep are raise-only, so a new day must force a
 *  reset+snap or the hero would stay stuck on yesterday's total until exceeded.
 *  A date-key (vs a magnitude heuristic) catches even low-volume days. */
let heroDateKey = "";
/** Whether a session was active on the previous tick — to detect the active→idle
 *  edge, where LiveCreep reconciles the override DOWN to the exact real total. */
let heroWasActive = false;

function localDateKey(now: number): string {
  const d = new Date(now);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

/** Feed the creep the latest real total + activity and paint the hero. `first`
 *  snaps (no roll) on the very first paint; later ticks roll continuously.
 *
 *  Two DOWNWARD cases need a snap (the raise-only `setTarget` would otherwise
 *  freeze the reel above the real value):
 *   • LOCAL-MIDNIGHT rollover — the local date changed, so "today" reset to ~0:
 *     reset the creep + snap down to the new day's total.
 *   • ACTIVE→IDLE — when the last session goes idle, LiveCreep reconciles its
 *     projected lead back down to the exact real total: snap to that total
 *     (mirrors the dashboard's `creepWasActive && !anySessionActive` path). */
function updateHero(realTotal: number, first: boolean): void {
  const now = Date.now();
  const dateKey = localDateKey(now);
  const dayRolled = !first && dateKey !== heroDateKey && heroDateKey !== "";
  heroDateKey = dateKey;

  const wasActive = heroWasActive;
  heroWasActive = anySessionActive;

  if (dayRolled) {
    liveCreep.reset(realTotal, now);
    liveCreep.setActive(anySessionActive);
    renderHero(realTotal, "snap"); // snap DOWN to the new day's total
    return;
  }

  liveCreep.observeReal(realTotal, now);
  liveCreep.setActive(anySessionActive);
  const override = liveCreep.tick(now);

  if (!first && wasActive && !anySessionActive) {
    // Active→idle: the creep just reconciled down to the exact real total. Snap
    // the reel down (setTarget is raise-only and would otherwise stay stuck at
    // the projected lead).
    renderHero(realTotal, "snap");
    return;
  }

  // While active, show the creeping projection; idle holds the exact real total.
  const value = anySessionActive ? override : realTotal;
  renderHero(value, first ? "snap" : "roll");
}

// --- Codex session (tier ≥ 2) ----------------------------------------------

function codexSignature(limits: Limits | null): string {
  const codex = limits?.codex;
  if (!codex || !codex.session) return "empty";
  const five = codex.fiveHour?.usedPercent;
  const weekly = codex.weekly?.usedPercent;
  return [
    codex.session.model,
    codex.session.tokens,
    five ?? "",
    weekly ?? "",
  ].join("|");
}

function renderCodex(limits: Limits | null): void {
  const winsEl = el("pop-codex-windows");
  // Skip the rebuild when the rendered Codex content is unchanged: the windows'
  // `innerHTML` was rebuilt on every tick, flickering the bars + jittering the
  // height. Only rebuild when the model/tokens/5h%/weekly% signature changes.
  const sig = codexSignature(limits);
  if (sig === lastCodexSig && winsEl.childElementCount > 0) return;
  lastCodexSig = sig;

  const modelEl = el("pop-codex-model");
  const tokEl = el("pop-codex-tok");
  const codex = limits?.codex;

  if (!codex || !codex.session) {
    modelEl.textContent = "No active Codex session";
    modelEl.classList.add("pop-codex-empty");
    tokEl.textContent = "";
    winsEl.innerHTML = "";
    return;
  }
  modelEl.classList.remove("pop-codex-empty");
  modelEl.textContent = prettyModel(codex.session.model);
  tokEl.textContent = `${formatTokens(codex.session.tokens)} tok`;

  const win = (lbl: string, pct: number | null | undefined): string => {
    if (pct === null || pct === undefined || Number.isNaN(pct)) return "";
    const used = Math.max(0, Math.min(100, pct));
    return `
      <div class="pop-win">
        <span class="pop-win-lbl">${lbl}</span>
        <span class="pop-win-bar"><i style="width:${used.toFixed(0)}%"></i></span>
        <span class="pop-win-pct">${used.toFixed(0)}%</span>
      </div>`;
  };
  winsEl.innerHTML =
    win("5h", codex.fiveHour?.usedPercent) + win("Weekly", codex.weekly?.usedPercent);
}

// --- Top-3 models (tier ≥ 3) ------------------------------------------------

/** Reconcile the top-3 model rows: keyed by model id so bars + counts persist
 *  across refreshes; new rows fade in, dropped rows fade out. (Same look as the
 *  old popover.) */
function renderModels(list: HTMLElement, models: ByModel[]): void {
  const top = models.slice(0, TOP_N);
  if (top.length === 0) {
    list.innerHTML = `<li class="pop-model-empty">No model usage yet today</li>`;
    lastModelTokens.clear();
    return;
  }
  const placeholder = list.querySelector(".pop-model-empty");
  if (placeholder) placeholder.remove();

  const max = Math.max(...top.map((m) => m.tokens), 1);
  const seen = new Set<string>();

  top.forEach((m, i) => {
    seen.add(m.model);
    const color = RANK_COLORS[i] ?? RANK_COLORS[RANK_COLORS.length - 1];
    let row = list.querySelector<HTMLElement>(
      `.pop-model-row[data-model="${cssAttrEscape(m.model)}"]`,
    );
    if (!row) {
      row = document.createElement("li");
      row.className = "pop-model-row entering";
      row.dataset.model = m.model;
      row.innerHTML = `
        <span class="pop-dot"></span>
        <span class="pop-name"></span>
        <span class="pop-tok"></span>
        <span class="pop-bar"><i></i></span>`;
      list.appendChild(row);
      requestAnimationFrame(() => row?.classList.remove("entering"));
    }
    list.appendChild(row); // re-append in rank order

    const dot = row.querySelector<HTMLElement>(".pop-dot")!;
    const name = row.querySelector<HTMLElement>(".pop-name")!;
    const tok = row.querySelector<HTMLElement>(".pop-tok")!;
    const fill = row.querySelector<HTMLElement>(".pop-bar > i")!;

    dot.style.background = color;
    dot.style.boxShadow = `0 0 8px 1px ${color}`;
    fill.style.background = color;
    name.textContent = prettyModel(m.model);
    lastModelTokens.set(m.model, m.tokens);
    tok.textContent = formatTokens(m.tokens);
    fill.style.width = `${Math.max(5, Math.round((m.tokens / max) * 100))}%`;
  });

  for (const row of [...list.querySelectorAll<HTMLElement>(".pop-model-row")]) {
    const id = row.dataset.model ?? "";
    if (!seen.has(id)) {
      lastModelTokens.delete(id);
      row.classList.add("leaving");
      window.setTimeout(() => row.remove(), LEAVE_MS);
    }
  }
}

// --- Live sessions (tier ≥ 4) ----------------------------------------------

function sessionsSignature(sessions: SessionState[]): string {
  if (!sessions.length) return "empty";
  // NOTE: `updatedAt` is deliberately EXCLUDED — it changes every tick, and
  // including it would rebuild the list each refresh (the flicker + height
  // jitter this signature exists to prevent). The freshness label is updated in
  // place by refreshSessionFreshness() instead.
  return sessions
    .map((s) => {
      const tool = s.state.kind === "working" ? (s.state.tool ?? "") : "";
      return `${s.sessionId}|${s.state.kind}|${tool}|${lastUserMessageOf(s)}|${s.model}|${sessionSource(s)}`;
    })
    .join("\n");
}

function renderSessions(list: HTMLElement, sessions: SessionState[]): void {
  // Skip the DOM rebuild when nothing rendered actually changed — otherwise the
  // 1.5s heartbeat + every SSE tick re-`innerHTML` the list, which flickers the
  // rows and jitters the height (re-triggering auto-fit → the window jumps).
  const sig = sessionsSignature(sessions);
  if (sig === lastSessionsSig && list.childElementCount > 0) {
    // Rows are reused (the signature deliberately ignores updatedAt + tokens so a
    // same-session activity tick doesn't rebuild + reflow the list). But those two
    // DO change per tick, so patch them in place: refresh each row's `data-updated`
    // stamp + token text, THEN recompute the freshness from the fresh stamps.
    patchReusedRows(list, sessions);
    refreshSessionFreshness(list);
    return;
  }
  lastSessionsSig = sig;

  if (!sessions.length) {
    list.innerHTML = `<li class="pop-sess-empty">No active sessions</li>`;
    return;
  }
  // Two lines per row (per the design pass): line 1 is source icon + the last
  // user message + freshness; line 2 is the state pill + dimmed model + tokens.
  // The project name is removed. The text source badge is replaced by the icon
  // (the pill cost too much width at 11.5px).
  list.innerHTML = sessions
    .map((s) => {
      const active = isActiveState(s.state) ? " active" : "";
      const source = sessionSource(s);
      const hasMsg = lastUserMessageOf(s).length > 0;
      const updated = Number.isFinite(s.updatedAt) ? s.updatedAt : "";
      return `
        <li class="pop-sess-row">
          <div class="pop-sess-top">
            ${sourceIconSvg(source, "pop-sess-icon")}
            <span class="pop-sess-msg${hasMsg ? "" : " pop-sess-msg-empty"}">${escapeHtml(sessionMessage(s))}</span>
            <span class="pop-sess-fresh" data-updated="${updated}">${escapeHtml(formatFreshness(s.updatedAt))}</span>
          </div>
          <div class="pop-sess-meta">
            <span class="pop-sess-state pop-sess-state-${s.state.kind}"><span class="pop-sess-dot${active}"></span>${escapeHtml(stateLabel(s.state))}</span>
            <span class="pop-sess-model">${escapeHtml(prettyModel(s.model))}</span>
            <span class="pop-sess-tok">${formatTokens(s.tokens)} tok</span>
          </div>
        </li>`;
    })
    .join("");
}

/** Patch the per-tick-mutable fields (last-activity stamp + token count) on
 *  reused rows. Safe to align by index: this only runs when the signature
 *  matched, which pins session identity + order. Without this the token count
 *  and the freshness BASELINE (`data-updated`) would freeze at the values from
 *  the last full rebuild, so "Ns ago" would drift and tokens would go stale. */
function patchReusedRows(list: HTMLElement, sessions: SessionState[]): void {
  const rows = list.querySelectorAll<HTMLElement>(".pop-sess-row");
  if (rows.length !== sessions.length) return; // shape drifted — let the next rebuild fix it
  sessions.forEach((s, i) => {
    const row = rows[i];
    const fresh = row.querySelector<HTMLElement>(".pop-sess-fresh");
    if (fresh) {
      fresh.setAttribute("data-updated", Number.isFinite(s.updatedAt) ? String(s.updatedAt) : "");
    }
    const tok = row.querySelector<HTMLElement>(".pop-sess-tok");
    if (tok) tok.textContent = `${formatTokens(s.tokens)} tok`;
  });
}

/** Refresh each visible session row's freshness text from its `data-updated`
 *  epoch stamp — called on every refresh so "Ns ago" advances live without
 *  rebuilding the rows (which would flicker + re-fit the window). */
function refreshSessionFreshness(list: HTMLElement, nowMs: number = Date.now()): void {
  list.querySelectorAll<HTMLElement>(".pop-sess-fresh[data-updated]").forEach((node) => {
    const raw = node.getAttribute("data-updated");
    if (!raw) return;
    node.textContent = formatFreshness(Number(raw), nowMs);
  });
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

// --- size adaptation (ResizeObserver) --------------------------------------

/** Map a popover WIDTH to the reel mantissa digit budget (graduated precision). */
function digitsForWidth(width: number): number {
  for (const t of PRECISION_TIERS) {
    if (width >= t.minWidth) return t.maxDigits;
  }
  return PRECISION_TIERS[PRECISION_TIERS.length - 1].maxDigits;
}

/** Apply the height → content tier + width → hero precision from the current
 *  popover box. Re-paints the hero immediately when the precision budget changes
 *  so the reel reflow is instant (snap, since the mantissa/suffix jumps).
 *
 *  The tier is ALWAYS recomputed from `height` (the window height the user
 *  dragged to is the authoritative input for how much content shows). This used
 *  to be gated behind an `autoFitting` flag to stop the auto-fit's own resize
 *  from feeding back into the tier, but that flag RACED the startup ResizeObserver
 *  fires and could leave the tier stuck low (reveals hidden) while the window was
 *  tall → dead space. The feedback it guarded against no longer exists now that
 *  `naturalContentHeight` measures correctly: each tier's fitted content height
 *  sits cleanly INSIDE that tier's band (tier-4 content ≈361 ≥300; tier-3 content
 *  ≈292 <300), so a window auto-fitted to a tier maps straight back to the same
 *  tier — no oscillation. */
function applySize(root: HTMLElement, width: number, height: number, lastReal: number): void {
  let tier = 1;
  if (height >= TIER_SESSIONS_PX) tier = 4;
  else if (height >= TIER_MODELS_PX) tier = 3;
  else if (height >= TIER_CODEX_PX) tier = 2;
  root.dataset.tier = String(tier);
  applyTierReveals(root, tier);

  const maxDigits = digitsForWidth(width);
  if (maxDigits !== heroMaxDigits) {
    heroMaxDigits = maxDigits;
    // The mantissa/suffix scale changes, so snap to the current value rather than
    // rolling (a scale switch makes the integer jump — setTarget is raise-only
    // and would ignore a drop).
    const value = anySessionActive ? liveCreep.value() : lastReal;
    renderHero(value, "snap");
  }
  // After precision settles, ensure the hero never overflows the card at this
  // width (measurement-based clamp; no-op in jsdom where widths are 0).
  fitHeroToWidth(lastReal);
}

/** Section → the minimum tier at which it reveals. Codex (with its 5h/weekly
 *  LIMIT bars) appears at tier 2 — BEFORE the top-models (tier 3) and live
 *  sessions (tier 4) — per the required priority order (fix #6). */
const SECTION_MIN_TIER: Record<string, number> = {
  codex: 2,
  models: 3,
  sessions: 4,
};

/** Toggle `.is-visible` on each reveal wrapper for the given tier. Adding the
 *  class animates the grid-rows collapse open (and removing it closes it); it is
 *  also the query target for the auto-fit height measurement. */
function applyTierReveals(root: HTMLElement, tier: number): void {
  for (const reveal of root.querySelectorAll<HTMLElement>(".pop-reveal")) {
    const section = reveal.dataset.section ?? "";
    const min = SECTION_MIN_TIER[section] ?? 99;
    reveal.classList.toggle("is-visible", tier >= min);
  }
}

// --- hero overflow guard (measurement-based, fix #1) -----------------------
// `digitsForWidth` gives a deterministic FIRST cut from the width tiers (kept so
// the unit tests stay stable in jsdom). On top of that, in the REAL webview we
// measure the rendered hero row and, if it still overruns the card's inner
// width, step the precision down further (more compaction) and finally scale the
// font down — so a freakishly wide value (or a font/locale quirk) can NEVER
// collide with the "tokens" label or breach the safe-area padding.

/** Smallest hero font scale we will shrink to before relying on the (always-on)
 *  overflow clip. Keeps the number legible. */
const HERO_MIN_SCALE = 0.62;

/** Measure the hero row vs its available width and compact/scale until it fits.
 *  Safe in jsdom (all widths read 0 → the `avail<=0` guard returns early). */
function fitHeroToWidth(lastReal: number): void {
  const hero = document.getElementById("pop-hero");
  const unit = hero?.querySelector<HTMLElement>(".pop-hero-unit");
  const numEl = document.getElementById("pop-hero-num");
  if (!hero || !numEl) return;

  const avail = heroAvailWidth(hero, unit);
  if (avail <= 0) return; // no layout (jsdom/hidden) — nothing to measure

  // Reset any prior scale so we measure at full size first.
  setHeroScale(1);

  // 1) Step the digit budget down until the natural width fits (or we bottom
  //    out at the tightest 2-digit budget). Each step re-renders the reel.
  let guard = 0;
  while (numEl.scrollWidth > avail && heroMaxDigits > 2 && guard++ < 6) {
    heroMaxDigits = nextTighterBudget(heroMaxDigits);
    const value = anySessionActive ? liveCreep.value() : lastReal;
    renderHero(value, "snap");
  }

  // 2) Still too wide at the tightest budget (e.g. a very narrow window): scale
  //    the font down proportionally, floored at HERO_MIN_SCALE.
  if (numEl.scrollWidth > avail) {
    const scale = Math.max(HERO_MIN_SCALE, avail / (numEl.scrollWidth || 1));
    setHeroScale(scale);
  }
}

/** Inner width available to the hero NUMBER: the hero box minus the "tokens"
 *  unit, its gap, and a comfortable safe-area cushion so the digits never kiss
 *  the label or the card edge. */
function heroAvailWidth(hero: HTMLElement, unit: HTMLElement | null | undefined): number {
  const SAFE_GAP = 12; // gap to the unit + breathing room
  const heroW = hero.clientWidth;
  const unitW = unit ? unit.offsetWidth : 0;
  return heroW - unitW - SAFE_GAP;
}

/** Apply a uniform horizontal scale to the reel + suffix (transform keeps the
 *  baseline + glow intact; width is the only thing we shrink). */
function setHeroScale(scale: number): void {
  document.documentElement.style.setProperty("--pop-hero-scale", String(scale));
}

/** Next tighter digit budget below `d` (99 → 6 → 4 → 3 → 2), for the overflow
 *  walk. Includes the 6-digit tier so the narrowing steps full → …K → …M → …B
 *  (the K representation is used at budget 6). */
function nextTighterBudget(d: number): number {
  if (d > 6) return 6;
  if (d > 4) return 4;
  if (d > 3) return 3;
  return 2;
}

// --- live opacity (Part 2) -------------------------------------------------

/** Apply the "Tray background" opacity (0..100) to the acrylic CSS tint alpha. */
function applyOpacity(pct: number): void {
  const alpha = Math.max(0, Math.min(100, pct)) / 100;
  document.documentElement.style.setProperty("--pop-alpha", String(alpha));
}

// --- frameless resize dragging (Part 3) ------------------------------------

/** Tauri's injected IPC bridge (present only inside the app webview). */
interface TauriInternals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}
function tauriInternals(): TauriInternals | null {
  const internals = (window as unknown as { __TAURI_INTERNALS__?: TauriInternals })
    .__TAURI_INTERNALS__;
  return internals && typeof internals.invoke === "function" ? internals : null;
}

// --- shared drag lifecycle (fix #1: drag freezes the whole monitor) ---------
// BOTH gestures — a resize-handle drag and a whole-card move-drag — funnel
// through one begin/end pair. While `dragging` is true NO heavy work runs (the
// RO flush early-returns; the heartbeat/SSE/auto-fit are gated; the reel rAF is
// halted). On release we settle the layout EXACTLY ONCE: one applySize + one
// refresh (which re-fits the height and resumes the reel's creep).

/** The popover root, captured for the drag-end settle (applySize needs the box). */
let dragRoot: HTMLElement | null = null;
/** End handler for the in-flight drag (so we can detach the global listeners). */
let dragEnd: (() => void) | null = null;

/** Last-resort backstop (ms) that ends a drag if EVERY genuine end signal
 *  (mouseup/pointerup/blur) was somehow swallowed. Must be LONG: a deliberate
 *  slow resize/move can easily run many seconds, and if this fires mid-drag it
 *  would re-enable the per-frame ResizeObserver work and the freeze could recur.
 *  The real end-events are reliable (the OS drag loop emits mouseup), so this is
 *  only a stuck-flag safety net, not a normal-drag timer. */
const DRAG_SAFETY_MS = 30_000;

/** Start a drag (resize OR move). Idempotent: a second begin while already
 *  dragging is ignored (so a move-mousedown can't re-arm during a resize). Halts
 *  the reel's creep rAF so it isn't painting while DWM recomposites the acrylic
 *  every frame, and arms the end signals (mouseup/pointerup/blur + a long safety
 *  timeout so a swallowed end-event can never leave updates frozen). */
function beginDrag(root: HTMLElement, kind: "move" | "resize" = "resize"): void {
  if (dragging) return;
  dragging = true;
  dragRoot = root;
  // HALT the reel's rAF so the odometer isn't painting during the drag.
  heroOdo?.snapTo(heroOdo.value());

  let safety = 0;
  let ended = false;
  const end = (): void => {
    if (ended) return;
    ended = true;
    dragging = false;
    dragEnd = null;
    if (typeof window.removeEventListener === "function") {
      window.removeEventListener("mouseup", end);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("blur", end);
    }
    if (safety) window.clearTimeout(safety);
    // Only a RESIZE drag changed the WINDOW size, so only then invalidate the
    // auto-fit memo to force a recompute + re-fit: a resize within the same tier
    // can land on the exact `lastFitH` value, and without this reset
    // `autoFitHeight` would early-return (its `< 2` no-op guard) and leave the
    // window at the raw dragged size — dead space / a slight clip instead of
    // hugging content (fix #5, post-drag case).
    //
    // A MOVE drag does NOT change the size, so it must NOT re-fit: keeping
    // `lastFitH` intact lets `autoFitHeight` no-op at the current height, so
    // dragging the popover to a new position never grows it (previously the move
    // settle reset the memo and snapped the height back to measured content,
    // which on macOS over-measures and crept the window taller every drag).
    if (kind === "resize") {
      lastFitH = 0;
    }
    // Settle ONCE: re-apply the tier/precision for the final box, then refresh
    // (re-fits the window height + resumes the reel). No per-frame work ran
    // during the drag, so this single pass is all the layout work there is.
    const node = dragRoot;
    dragRoot = null;
    if (node) applySize(node, node.clientWidth, node.clientHeight, lastRealTotal);
    void refresh();
  };
  dragEnd = end;
  if (typeof window.addEventListener === "function") {
    window.addEventListener("mouseup", end);
    window.addEventListener("pointerup", end);
    window.addEventListener("blur", end);
  }
  if (typeof window.setTimeout === "function") {
    safety = window.setTimeout(end, DRAG_SAFETY_MS);
  }
}

/** Force-end the in-flight drag (used when an IPC call rejects, so the live
 *  updates aren't left frozen). No-op when not dragging. */
function forceEndDrag(): void {
  dragEnd?.();
}

/** Wire BOTH drag gestures through the shared `beginDrag` lifecycle so neither
 *  one does per-frame work (fix #1):
 *   • the edge/corner handles → `start_resize_dragging` (a resize drag);
 *   • a mousedown on `#popover` itself → a MOVE drag (the data-tauri-drag-region
 *     moves the window natively; the handles `stopPropagation`, so this fires
 *     only for genuine move-drags / clicks on the body, never for resizes).
 *  In both cases `beginDrag` halts the reel + gates the live updates until the
 *  matching mouseup. No-op in a browser (no Tauri IPC bridge). */
function wireResize(root: HTMLElement): void {
  const internals = tauriInternals();
  if (!internals) return;

  // Move-drag: a plain left-button mousedown anywhere on the card body. The OS
  // moves the window via the data-tauri-drag-region; we just need to pause the
  // heavy work for the duration. Resize handles stopPropagation above this, so
  // this never double-fires for a resize. A bare click (mousedown→mouseup with
  // no move) also runs begin→end harmlessly (one settle pass on release).
  root.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    beginDrag(root, "move");
  });

  for (const handle of root.querySelectorAll<HTMLElement>(".pop-resize")) {
    handle.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      const dir = handle.dataset.resize;
      if (!dir) return;
      e.preventDefault();
      e.stopPropagation(); // don't also trigger the move-drag region
      beginDrag(root);
      // Tauri 2: the command parameter is `value` (a `ResizeDirection`), NOT
      // `direction` — verified against tauri-2.11.2 `setter!` macro + the
      // official @tauri-apps/api `startResizeDragging`. The data-resize values
      // ("North"/"NorthEast"/…) already match the PascalCase enum variants. If
      // the IPC rejects, force-end the drag so live updates aren't left frozen.
      internals
        .invoke("plugin:window|start_resize_dragging", { value: dir })
        .catch(() => forceEndDrag());
    });
  }
}

// --- auto-fit window height to content (fix #3 / #5) -----------------------
// The popover snaps its WINDOW height to its CONTENT height so there is never
// dead whitespace below the deepest visible section. The window height drives
// the TIER (how much content shows); auto-fit then snaps the window to exactly
// that tier's content. Growth is upward (the Rust command anchors the bottom
// edge, since the popover sits above the tray). We don't spam identical heights
// at the IPC (`lastFitH`). The tier↔fit loop is stable by construction (each
// tier's fitted content sits inside its band — see `applySize`), so no
// suppression flag is needed.

/** Vertical padding of #popover (top+bottom) — mirrors popover.css. The content
 *  box height + this = the window inner height we want. */
const POPOVER_V_PADDING = 33; // 17px top + 16px bottom
/** Inter-section spacing of a VISIBLE reveal — mirrors `.pop-reveal.is-visible`'s
 *  `margin-top` in popover.css. We use this CONSTANT rather than the element's
 *  live computed `margin-top`, because that property ANIMATES 0→14px on reveal;
 *  reading it mid-transition would under-measure the content height. The reveal's
 *  settled spacing is always this value, so the auto-fit lands on the final size
 *  even if it measures during the open animation. Keep in sync with the CSS. */
const REVEAL_GAP_PX = 14;

/** Measure the natural content height and ask the shell to fit the window to it.
 *  No-op outside the Tauri webview or when layout is unavailable (jsdom). */
function autoFitHeight(): void {
  const internals = tauriInternals();
  if (!internals) return;
  if (dragging) return; // never fight an in-flight user drag
  const content = document.getElementById("pop-content");
  if (!content) return;
  // Compute the FINAL content height (hero + each visible reveal's content +
  // the inter-section spacing) via scrollHeight + a constant gap, so it is
  // correct even MID reveal-animation (see naturalContentHeight). This makes the
  // window snap straight to the settled size with no animating-height wobble.
  const contentH = naturalContentHeight(content);
  if (contentH <= 0) return; // no layout yet
  const targetH = Math.round(contentH + POPOVER_V_PADDING);
  if (Math.abs(targetH - lastFitH) < 2) return; // already fitted
  lastFitH = targetH;

  // Best-effort. The resulting resize re-fires the ResizeObserver, which
  // recomputes the tier from the fitted height — that's fine and stable: each
  // tier's fitted content sits inside its own band (see `applySize`), so the
  // window maps straight back to the same tier (no oscillation, no need for an
  // `autoFitting` suppression flag, which previously raced the startup fires and
  // left the tier stuck low → dead space).
  internals.invoke("set_popover_height", { height: targetH }).catch(() => {
    /* ignore — best-effort; the CSS still reads fine if it fails */
  });
}

/** The settled height of the content column: the always-visible hero plus each
 *  currently-visible reveal section's natural CONTENT height, plus the column
 *  gap between every visible block.
 *
 *  CRITICAL: we read the inner's `scrollHeight`, NOT `getBoundingClientRect()`.
 *  `.pop-reveal-inner` is `overflow:hidden` inside a `grid-template-rows`
 *  collapse that animates `0fr→1fr`; its *rendered* (bounding-rect) height is
 *  therefore 0 while collapsed and only PARTIAL mid-open-animation. Measuring
 *  that fed a too-small height into the auto-fit on first paint → the window
 *  shrank → the ResizeObserver recomputed a lower tier → the reveals collapsed
 *  → their rendered height went to 0 → the popover got STUCK at hero-only height
 *  (the dead-space / collapse bug). `scrollHeight` is the full content height
 *  regardless of the clip/animation, so the auto-fit lands on the settled size
 *  immediately and the tier never collapses. */
function naturalContentHeight(content: HTMLElement): number {
  // The hero is always present at its natural height.
  const hero = content.querySelector<HTMLElement>(".pop-today");
  let total = hero ? hero.scrollHeight || hero.getBoundingClientRect().height : 0;

  // Each VISIBLE reveal adds its inter-section spacing PLUS its content height.
  // CRITICAL: `.pop-content` has NO `row-gap` — the spacing between sections is
  // the `margin-top: 14px` on each `.pop-reveal.is-visible` (it animates to/from
  // 0 with the collapse). The old code read `content`'s row-gap (always 0 here),
  // so it under-measured the column by 14px PER visible section → the auto-fit
  // window came out short and the bottom sections were cut off / read as dead
  // space. We add the SETTLED spacing (`REVEAL_GAP_PX`, NOT the live computed
  // `margin-top` which animates 0→14) plus the inner `scrollHeight` (content
  // height) — both animation/collapse independent, so the fit is correct even
  // if measured mid open-animation.
  for (const reveal of content.querySelectorAll<HTMLElement>(".pop-reveal.is-visible")) {
    const inner = reveal.querySelector<HTMLElement>(".pop-reveal-inner");
    if (!inner) continue;
    total += REVEAL_GAP_PX + inner.scrollHeight;
  }
  return total;
}

// --- data refresh ----------------------------------------------------------

let lastRealTotal = 0;
let firstPaint = true;

/** One refresh: today summary + sessions + limits + opacity/currency settings.
 *  Repaints every section; the hero rolls via the creep. */
async function refresh(): Promise<void> {
  const [today, sessions, limits, settings] = await Promise.all([
    getSummary("today"),
    getSessions(),
    getLimits(),
    getSettings(),
  ]);

  anySessionActive = sessions.some((s) => isActiveState(s.state));
  lastRealTotal = today.totals.tokens;
  currency = asCurrency(settings.currency);

  // CRITICAL (fix #1): a refresh that STARTED just before a drag began can
  // resolve mid-drag. The callers are gated, but this in-flight promise is not —
  // and the painting below (updateHero → renderHero "roll" → heroOdo.setTarget,
  // which RESTARTS the reel rAF) plus the DOM renders + autoFitHeight would run
  // heavy work during the drag. The cheap data vars above are already refreshed
  // (so the drag-end settle has fresh values); bail before any painting. The
  // drag-end refresh() repaints everything once on release.
  if (dragging) return;

  applyOpacity(settings.popoverOpacity);

  updateHero(lastRealTotal, firstPaint);
  firstPaint = false;

  // Today's spend, in the user's currency (all costs are USD → convert).
  el("pop-spend").textContent = formatCost(today.totals.costUsd, currency, rates);

  renderCodex(limits);
  renderModels(el("pop-models"), today.byModel);
  renderSessions(el("pop-sessions"), sessions);

  // Content may have changed height (Codex session appeared, model list grew,
  // sessions list changed). Re-fit the window so there's no dead space — a
  // no-op when the height is unchanged (guarded by `lastFitH`) and while the
  // user is mid-drag. Deferred a frame so reveal transitions have laid out.
  if (typeof requestAnimationFrame === "function") {
    requestAnimationFrame(() => autoFitHeight());
  }
}

async function bootstrap(): Promise<void> {
  const root = el("popover");
  root.innerHTML = skeleton();
  root.dataset.tier = "1";
  // Fresh skeleton → drop any prior render signatures so the first refresh paints.
  lastSessionsSig = "";
  lastCodexSig = "";

  // Currency rates (cached daily server-side) for the spend display.
  try {
    const fx = await getFx();
    rates = fx.rates ?? {};
  } catch {
    rates = {};
  }

  wireResize(root);

  // Size adaptation: measure the popover box and set tier/precision. The RO
  // callback is THROTTLED to one rAF (coalesces the burst of frames a drag
  // produces). CRITICAL (fix #1): while `dragging` the flush EARLY-RETURNS — it
  // runs NO applySize / fitHeroToWidth / renderHero, so there is zero per-frame
  // layout/reel work to saturate the compositor against the acrylic recomposite.
  // The single settle pass happens on drag END (beginDrag's `end`).
  if (typeof ResizeObserver !== "undefined") {
    let roScheduled = false;
    let pendingW = 0;
    let pendingH = 0;
    const flush = (): void => {
      roScheduled = false;
      // No heavy work mid-drag; drag-END runs the authoritative settle. A flush
      // that was rAF-scheduled during the drag may fire just AFTER release (when
      // `dragging` is already false) — that re-runs applySize once more with the
      // final box. It's a cheap idempotent no-op (same dims the drag-end settle
      // used; tier/precision unchanged), and crucially it CANNOT run during the
      // drag, so the freeze guarantee (zero per-frame work while dragging) holds.
      if (dragging) return;
      applySize(root, pendingW, pendingH, lastRealTotal);
    };
    const ro = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect;
      if (!box) return;
      pendingW = box.width;
      pendingH = box.height;
      if (roScheduled) return;
      roScheduled = true;
      if (typeof requestAnimationFrame === "function") requestAnimationFrame(flush);
      else flush();
    });
    ro.observe(root);
  }
  // Establish the tier + reveals SYNCHRONOUSLY now, from the current box, BEFORE
  // the first refresh (whose rAF calls autoFitHeight). The ResizeObserver
  // callback is async (next frame), so without this the first autoFitHeight
  // could run while the reveals are still hidden (tier 1) → measure hero-only →
  // shrink the window to ~116 → the echo RO fire then sees that small height →
  // tier 1 → reveals stay collapsed → the popover gets STUCK hero-only. Setting
  // the tier up-front makes the first measurement see the real (tier-4) content,
  // so the auto-fit lands on the full height deterministically (fix #5).
  applySize(root, root.clientWidth, root.clientHeight, lastRealTotal);

  await refresh();

  // Live: refresh on token (`usage`) and session-state (`sessions`) ticks.
  // Skipped while ANY drag is in flight (the drag's end does the final refresh)
  // so the drag isn't competing with network + full re-render + reel repaint.
  subscribe((ev: CmServerEvent) => {
    if (dragging) return;
    if (ev.type === "usage" || ev.type === "sessions") void refresh();
  });

  // Steady heartbeat keeps the hero reel rolling continuously while a session
  // is active (same creep semantics as the dashboard) and refreshes content.
  // Paused mid-drag so the per-tick network + re-render + reel repaint don't
  // fight the drag (and can't saturate the compositor against the acrylic).
  if (typeof setInterval === "function") {
    setInterval(() => {
      if (!dragging) void refresh();
    }, HEARTBEAT_MS);
    // Cheap 1Hz freshness tick so each session row's "Ns ago" advances live
    // between the (1.5s) content refreshes. Text-only; gated mid-drag.
    setInterval(() => {
      if (dragging) return;
      const list = document.getElementById("pop-sessions");
      if (list) refreshSessionFreshness(list);
    }, 1_000);
  }
}

function autostart(): void {
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

export {
  renderModels,
  renderSessions,
  renderCodex,
  renderHero,
  updateHero,
  applySize,
  bootstrap,
};
