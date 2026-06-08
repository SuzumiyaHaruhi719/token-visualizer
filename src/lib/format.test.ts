import { describe, it, expect } from "vitest";
import { formatTokens, formatCost, formatPct, formatInt } from "./format";

describe("formatTokens", () => {
  it("shows small counts as plain integers", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
  });

  it("formats thousands with K", () => {
    expect(formatTokens(48_200)).toBe("48.2K");
    expect(formatTokens(1_000)).toBe("1K");
  });

  it("formats millions to ~3 sig figs", () => {
    expect(formatTokens(1_240_000)).toBe("1.24M");
    expect(formatTokens(48_200_000)).toBe("48.2M");
    expect(formatTokens(1_000_000)).toBe("1M");
  });

  it("formats billions and trillions", () => {
    expect(formatTokens(2_500_000_000)).toBe("2.5B");
    expect(formatTokens(3_000_000_000_000)).toBe("3T");
  });

  it("returns dash for null/undefined/NaN", () => {
    expect(formatTokens(null)).toBe("—");
    expect(formatTokens(undefined)).toBe("—");
    expect(formatTokens(NaN)).toBe("—");
  });
});

describe("formatCost", () => {
  it("formats with two decimals and a dollar sign", () => {
    expect(formatCost(4.1)).toBe("$4.10");
    expect(formatCost(0)).toBe("$0.00");
    expect(formatCost(41.073)).toBe("$41.07");
  });

  it("adds thousands separators", () => {
    expect(formatCost(1234.5)).toBe("$1,234.50");
  });

  it("returns dash for null", () => {
    expect(formatCost(null)).toBe("—");
    expect(formatCost(undefined)).toBe("—");
  });
});

describe("formatPct", () => {
  it("rounds a 0..1 ratio to an integer percent", () => {
    expect(formatPct(0.857)).toBe("86%");
    expect(formatPct(0)).toBe("0%");
    expect(formatPct(1)).toBe("100%");
  });

  it("returns dash for null", () => {
    expect(formatPct(null)).toBe("—");
    expect(formatPct(NaN)).toBe("—");
  });
});

describe("formatInt", () => {
  it("shows zero and small counts exactly", () => {
    expect(formatInt(0)).toBe("0");
    expect(formatInt(37)).toBe("37");
  });
  it("adds thousands separators", () => {
    expect(formatInt(12345)).toBe("12,345");
    expect(formatInt(14_686_055_640)).toBe("14,686,055,640");
  });
  it("rounds fractional (mid-tween) values to the ones digit", () => {
    expect(formatInt(1234.4)).toBe("1,234");
    expect(formatInt(1234.6)).toBe("1,235");
  });
  it("returns dash for null/undefined/NaN", () => {
    expect(formatInt(null)).toBe("—");
    expect(formatInt(undefined)).toBe("—");
    expect(formatInt(NaN)).toBe("—");
  });
});
