import { describe, it, expect } from "vitest";
import {
  convert,
  formatCost,
  asCurrency,
  currencySymbol,
  CURRENCIES,
  type FxRates,
} from "./currency";

// A representative USD-based rate table (X per 1 USD).
const RATES: FxRates = {
  CNY: 7.2,
  HKD: 7.8,
  EUR: 0.92,
  JPY: 150,
  GBP: 0.79,
};

describe("convert", () => {
  it("passes USD through unchanged", () => {
    expect(convert(41.07, "USD", RATES)).toBe(41.07);
  });

  it("multiplies by the USD-based rate for non-USD", () => {
    expect(convert(10, "CNY", RATES)).toBeCloseTo(72, 6);
    expect(convert(10, "EUR", RATES)).toBeCloseTo(9.2, 6);
    expect(convert(2, "JPY", RATES)).toBeCloseTo(300, 6);
  });

  it("falls back to the USD value when the rate is missing/invalid", () => {
    expect(convert(10, "CNY", {})).toBe(10);
    expect(convert(10, "CNY", { CNY: 0 })).toBe(10);
    expect(convert(10, "CNY", { CNY: Number.NaN })).toBe(10);
  });

  it("returns null for null/undefined/NaN input", () => {
    expect(convert(null, "CNY", RATES)).toBeNull();
    expect(convert(undefined, "CNY", RATES)).toBeNull();
    expect(convert(Number.NaN, "CNY", RATES)).toBeNull();
  });
});

describe("formatCost", () => {
  it("formats USD with US$ and two decimals", () => {
    expect(formatCost(4.1, "USD", RATES)).toBe("US$4.10");
    expect(formatCost(0, "USD", RATES)).toBe("US$0.00");
    expect(formatCost(1234.5, "USD", RATES)).toBe("US$1,234.50");
  });

  it("converts and prefixes the right symbol per currency", () => {
    expect(formatCost(10, "CNY", RATES)).toBe("¥72.00");
    expect(formatCost(10, "HKD", RATES)).toBe("HK$78.00");
    expect(formatCost(10, "EUR", RATES)).toBe("€9.20");
    expect(formatCost(100, "GBP", RATES)).toBe("£79.00");
  });

  it("renders JPY with no decimals (no minor unit) and the JP¥ symbol", () => {
    expect(formatCost(10, "JPY", RATES)).toBe("JP¥1,500");
    expect(formatCost(1.5, "JPY", RATES)).toBe("JP¥225");
  });

  it("returns a dash for null/undefined", () => {
    expect(formatCost(null, "CNY", RATES)).toBe("—");
    expect(formatCost(undefined, "USD", RATES)).toBe("—");
  });

  it("defaults to USD with an empty rate table", () => {
    expect(formatCost(5)).toBe("US$5.00");
  });
});

describe("asCurrency", () => {
  it("accepts known codes and rejects unknown ones", () => {
    expect(asCurrency("CNY")).toBe("CNY");
    expect(asCurrency("usd")).toBe("USD"); // case-sensitive → unknown → default
    expect(asCurrency("XYZ")).toBe("USD");
    expect(asCurrency(null)).toBe("USD");
    expect(asCurrency(undefined)).toBe("USD");
  });
});

describe("currencySymbol", () => {
  it("returns a non-empty symbol for every supported currency", () => {
    for (const c of CURRENCIES) {
      expect(currencySymbol(c).length).toBeGreaterThan(0);
    }
  });
});
