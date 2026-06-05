// Formatting helpers for the dashboard.

const TRILLION = 1_000_000_000_000;
const BILLION = 1_000_000_000;
const MILLION = 1_000_000;
const THOUSAND = 1_000;

/**
 * Format a token count compactly: 1_240_000 -> "1.24M", 48_200_000 -> "48.2M".
 * Keeps roughly 3 significant figures. Plain integers below 1000 are shown as-is.
 */
export function formatTokens(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  const abs = Math.abs(n);
  if (abs < THOUSAND) return String(Math.round(n));

  const pick = (value: number, suffix: string): string => {
    // 3 significant figures: <10 -> 2 decimals, <100 -> 1 decimal, else 0.
    let decimals: number;
    if (Math.abs(value) < 10) decimals = 2;
    else if (Math.abs(value) < 100) decimals = 1;
    else decimals = 0;
    const fixed = value.toFixed(decimals);
    // Strip trailing zeros after the decimal point (1.20M -> 1.2M, 1.00M -> 1M).
    const trimmed = decimals > 0 ? fixed.replace(/\.?0+$/, "") : fixed;
    return `${trimmed}${suffix}`;
  };

  if (abs >= TRILLION) return pick(n / TRILLION, "T");
  if (abs >= BILLION) return pick(n / BILLION, "B");
  if (abs >= MILLION) return pick(n / MILLION, "M");
  return pick(n / THOUSAND, "K");
}

/**
 * Format a USD cost: 4.1 -> "$4.10", null -> "—".
 * Large values get thousands separators.
 */
export function formatCost(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) return "—";
  return (
    "$" +
    value.toLocaleString("en-US", {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    })
  );
}

/**
 * Format a 0..1 ratio as an integer percentage: 0.857 -> "86%".
 * Accepts null/undefined -> "—".
 */
export function formatPct(ratio: number | null | undefined): string {
  if (ratio === null || ratio === undefined || Number.isNaN(ratio)) return "—";
  return `${Math.round(ratio * 100)}%`;
}

/** Format an integer with thousands separators: 12345 -> "12,345". */
export function formatInt(n: number | null | undefined): string {
  if (n === null || n === undefined || Number.isNaN(n)) return "—";
  return Math.round(n).toLocaleString("en-US");
}
