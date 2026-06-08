// Compact "Claude X% · Codex Y%" split bar driven by summary.bySource.
// A thin two-segment bar plus two labelled chips, sitting under the KPI row.

import type { BySource } from "../lib/types";
import { formatPct } from "../lib/format";

const SOURCE_LABEL: Record<BySource["source"], string> = {
  claude: "Claude",
  codex: "Codex",
};

/** Render the by-source split into `container`. Empty data → graceful dash. */
export function renderBySource(container: HTMLElement, bySource: BySource[]): void {
  const rows = Array.isArray(bySource) ? bySource.filter((b) => b && b.tokens >= 0) : [];
  const total = rows.reduce((sum, b) => sum + (b.tokens || 0), 0);

  if (rows.length === 0 || total <= 0) {
    container.innerHTML = `<span class="bysource-empty">No source data</span>`;
    return;
  }

  const segments = rows
    .map((b) => {
      const frac = b.tokens / total;
      return `<div class="bysource-seg bysource-${b.source}" style="width:${(frac * 100).toFixed(2)}%"></div>`;
    })
    .join("");

  const chips = rows
    .map((b) => {
      const frac = b.tokens / total;
      return `<span class="bysource-chip bysource-chip-${b.source}"><span class="bysource-swatch bysource-${b.source}"></span>${SOURCE_LABEL[b.source]} ${formatPct(frac)}</span>`;
    })
    .join("");

  container.innerHTML = `
    <div class="bysource-bar">${segments}</div>
    <div class="bysource-chips">${chips}</div>`;
}
