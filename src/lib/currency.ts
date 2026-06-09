// Currency conversion + formatting for cost displays.
//
// ALL backend cost values are USD. The backend fetches USD-based FX rates once
// per day (see src-tauri/src/fx.rs, served at GET /api/fx) and the frontend
// converts on display. This module is PURE (no fetch, no DOM) so it unit-tests
// trivially with a rates map; the live rates are injected by the caller.

/** Supported display currencies. USD is always available (rate 1, no lookup). */
export type CurrencyCode = "USD" | "CNY" | "HKD" | "EUR" | "JPY" | "GBP";

/** The full set, in the order the settings <select> presents them. */
export const CURRENCIES: readonly CurrencyCode[] = [
  "USD",
  "CNY",
  "HKD",
  "EUR",
  "JPY",
  "GBP",
] as const;

/** A USD-based rate table: `rates[X]` = how many X per 1 USD. USD itself is implicitly 1. */
export type FxRates = Partial<Record<string, number>>;

/** The `/api/fx` payload shape (mirrors the Rust `FxResponse`). */
export interface FxPayload {
  base: string;
  rates: FxRates;
  /** Epoch seconds the rates were fetched (or last attempted). */
  fetchedAt: number;
  /** True when these are cached rates older than the refresh window (offline). */
  stale?: boolean;
}

/** Symbol prefix per currency. CNY and JPY both use ¥, disambiguated by code. */
const SYMBOLS: Record<CurrencyCode, string> = {
  USD: "US$",
  CNY: "¥",
  HKD: "HK$",
  EUR: "€",
  JPY: "JP¥",
  GBP: "£",
};

/** How many fraction digits to render per currency. JPY has no minor unit. */
const FRACTION_DIGITS: Record<CurrencyCode, number> = {
  USD: 2,
  CNY: 2,
  HKD: 2,
  EUR: 2,
  JPY: 0,
  GBP: 2,
};

/** Narrow an arbitrary string to a known {@link CurrencyCode}, defaulting to USD. */
export function asCurrency(code: string | null | undefined): CurrencyCode {
  return (CURRENCIES as readonly string[]).includes(code ?? "")
    ? (code as CurrencyCode)
    : "USD";
}

/**
 * Convert a USD amount into `currency` using a USD-based `rates` map.
 * USD passes through unchanged. A missing rate falls back to USD (rate 1) so a
 * partial/offline rate table never blanks out a cost. Returns null for null in.
 */
export function convert(
  usd: number | null | undefined,
  currency: CurrencyCode,
  rates: FxRates,
): number | null {
  if (usd === null || usd === undefined || Number.isNaN(usd)) return null;
  if (currency === "USD") return usd;
  const rate = rates[currency];
  if (rate === undefined || !Number.isFinite(rate) || rate <= 0) return usd; // no rate → show USD value
  return usd * rate;
}

/**
 * Format a USD cost into the target `currency` with the right symbol + decimals.
 * `null`/NaN → "—". This is the single cost formatter used across every cost
 * display (dashboard KPI, by-source, limits, popover spend).
 */
export function formatCost(
  usd: number | null | undefined,
  currency: CurrencyCode = "USD",
  rates: FxRates = {},
): string {
  const value = convert(usd, currency, rates);
  if (value === null) return "—";
  const digits = FRACTION_DIGITS[currency];
  return (
    SYMBOLS[currency] +
    value.toLocaleString("en-US", {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    })
  );
}

/** The display symbol for a currency (e.g. for a standalone label). */
export function currencySymbol(currency: CurrencyCode): string {
  return SYMBOLS[currency];
}
