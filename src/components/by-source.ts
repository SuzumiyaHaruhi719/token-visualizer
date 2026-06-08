// Compact "Claude X% / Codex Y%" split bar driven by summary.bySource.
// The bar keeps stable segment DOM between updates so width transitions have a
// real previous state to animate from instead of rebuilding at the final width.

import type { BySource } from "../lib/types";
import { formatPct } from "../lib/format";
import { animateHeightChange, revealOnNextFrame } from "../lib/motion";

const SOURCE_LABEL: Record<BySource["source"], string> = {
  claude: "Claude",
  codex: "Codex",
};
const SOURCE_ORDER: BySource["source"][] = ["claude", "codex"];
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

function ensureSplitMarkup(container: HTMLElement): void {
  if (container.querySelector(".bysource-bar")) return;
  container.innerHTML = splitMarkup();
  revealNewContent(container);
}

function updateSplit(container: HTMLElement, rows: BySource[], total: number): void {
  const totals: Record<BySource["source"], number> = { claude: 0, codex: 0 };
  for (const row of rows) totals[row.source] += row.tokens || 0;

  for (const source of SOURCE_ORDER) {
    const pct = total > 0 ? totals[source] / total : 0;
    const segment = container.querySelector<HTMLElement>(
      `.bysource-seg[data-source="${source}"]`,
    );
    const chip = container.querySelector<HTMLElement>(
      `.bysource-chip[data-source="${source}"]`,
    );
    const label = chip?.querySelector<HTMLElement>("[data-source-label]");
    if (segment) segment.style.width = `${(pct * 100).toFixed(2)}%`;
    if (chip) chip.classList.toggle("bysource-chip-muted", totals[source] <= 0);
    if (label) label.textContent = `${SOURCE_LABEL[source]} ${formatPct(pct)}`;
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
    ensureSplitMarkup(container);
    container.dataset.bysourceState = "split";
    updateSplit(container, rows, total);
  };
  if (container.dataset.bysourceState !== "split") {
    animateHeightChange(container, writeSplit);
  } else {
    writeSplit();
  }
}
