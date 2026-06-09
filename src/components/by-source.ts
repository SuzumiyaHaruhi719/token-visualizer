// Compact "Claude X% / Codex Y%" split bar driven by summary.bySource.
// The bar keeps stable segment DOM between updates so width transitions have a
// real previous state to animate from instead of rebuilding at the final width.

import type { BySource } from "../lib/types";
import { formatPct } from "../lib/format";
import { animateHeightChange, revealOnNextFrame } from "../lib/motion";

const SOURCE_LABEL: Record<BySource["source"], string> = {
  claude: "Claude",
  codex: "Codex",
  deepseek: "DeepSeek",
};
const SOURCE_ORDER: BySource["source"][] = ["claude", "codex", "deepseek"];
const ENTER_CLASS = "ui-enter";

function splitMarkup(): string {
  const segments = SOURCE_ORDER.map(
    (source) => `<div class="bysource-seg bysource-${source}" data-source="${source}" style="width:0%"></div>`,
  ).join("");
  const chips = SOURCE_ORDER.map(
    (source) =>
      `<span class="bysource-chip bysource-chip-${source}" data-source="${source}"><span class="bysource-swatch bysource-${source}"></span><span data-source-label>${SOURCE_LABEL[source]} 0%</span></span>`,
  ).join("");
  return `
    <div class="bysource-bar ${ENTER_CLASS}">${segments}</div>
    <div class="bysource-chips ${ENTER_CLASS}">${chips}</div>`;
}

function revealNewContent(container: HTMLElement): void {
  container.querySelectorAll(`.${ENTER_CLASS}`).forEach((node) => {
    revealOnNextFrame(node, ENTER_CLASS);
  });
}

/** Create the split markup if absent; returns true when it was just created
 *  (its segments start at width:0%, so the caller can stage the real widths on
 *  the next frame and let them grow from 0 instead of snapping). */
function ensureSplitMarkup(container: HTMLElement): boolean {
  if (container.querySelector(".bysource-bar")) return false;
  container.innerHTML = splitMarkup();
  revealNewContent(container);
  return true;
}

/** Apply the final segment width for one source. */
function setSegmentWidth(container: HTMLElement, source: BySource["source"], pct: number): void {
  const segment = container.querySelector<HTMLElement>(
    `.bysource-seg[data-source="${source}"]`,
  );
  if (segment) segment.style.width = `${(pct * 100).toFixed(2)}%`;
}

function updateSplit(
  container: HTMLElement,
  rows: BySource[],
  total: number,
  freshMarkup: boolean,
): void {
  const totals: Record<BySource["source"], number> = { claude: 0, codex: 0, deepseek: 0 };
  for (const row of rows) totals[row.source] += row.tokens || 0;

  const pctOf = (source: BySource["source"]): number =>
    total > 0 ? totals[source] / total : 0;

  for (const source of SOURCE_ORDER) {
    const pct = pctOf(source);
    const chip = container.querySelector<HTMLElement>(
      `.bysource-chip[data-source="${source}"]`,
    );
    const label = chip?.querySelector<HTMLElement>("[data-source-label]");
    if (chip) chip.classList.toggle("bysource-chip-muted", totals[source] <= 0);
    if (label) label.textContent = `${SOURCE_LABEL[source]} ${formatPct(pct)}`;
    // Existing bars transition from their current width immediately. Freshly
    // created bars start at 0%; defer the real width one frame so the CSS width
    // transition actually fires (0% -> final) instead of snapping pre-paint.
    if (!freshMarkup) setSegmentWidth(container, source, pct);
  }

  if (freshMarkup) {
    // Defer the real widths one frame so the CSS width transition fires (0% ->
    // final) and the segments GROW. We deliberately do NOT honor
    // prefers-reduced-motion here: Windows reports reduced motion whenever the
    // "animations" setting is off (very common in WebView2), which would
    // otherwise snap the bars and the user would never see them grow — the same
    // reasoning tween.ts documents for the odometer. Tests run rAF
    // synchronously, so the final width is still reached within the call.
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => {
        for (const source of SOURCE_ORDER) setSegmentWidth(container, source, pctOf(source));
      });
    } else {
      for (const source of SOURCE_ORDER) setSegmentWidth(container, source, pctOf(source));
    }
  }
}

/** Render the by-source split into `container`. Empty data gets a graceful dash. */
export function renderBySource(container: HTMLElement, bySource: BySource[]): void {
  const rows = Array.isArray(bySource) ? bySource.filter((b) => b && b.tokens >= 0) : [];
  const total = rows.reduce((sum, b) => sum + (b.tokens || 0), 0);

  if (rows.length === 0 || total <= 0) {
    const writeEmpty = (): void => {
      container.innerHTML = `<span class="bysource-empty ${ENTER_CLASS}">No source data</span>`;
      container.dataset.bysourceState = "empty";
      revealNewContent(container);
    };
    if (container.dataset.bysourceState !== "empty") {
      animateHeightChange(container, writeEmpty);
    } else {
      writeEmpty();
    }
    return;
  }

  const writeSplit = (): void => {
    const freshMarkup = ensureSplitMarkup(container);
    container.dataset.bysourceState = "split";
    updateSplit(container, rows, total, freshMarkup);
  };
  if (container.dataset.bysourceState !== "split") {
    animateHeightChange(container, writeSplit);
  } else {
    writeSplit();
  }
}
