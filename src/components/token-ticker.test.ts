import { describe, it, expect, beforeEach } from "vitest";
import { renderTokenTicker, updateTokenTicker } from "./token-ticker";
import { mockSummary } from "../lib/api";
import type { Summary } from "../lib/types";

function container(): HTMLElement {
  return document.createElement("div");
}

beforeEach(() => {
  // Snap number tweens instantly so ticker text is final right after render
  // (same reduced-motion stub used by the dashboard tests).
  (window as unknown as { matchMedia: (q: string) => unknown }).matchMedia = (q: string) => ({
    matches: true,
    media: q,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    onchange: null,
    dispatchEvent() {
      return false;
    },
  });
});

describe("renderTokenTicker", () => {
  it("renders the exact total with thousands separators", () => {
    const root = container();
    const summary = mockSummary("today");
    renderTokenTicker(root, summary);
    expect(root.querySelector("#ticker-total")?.textContent).toBe(
      summary.totals.tokens.toLocaleString("en-US"),
    );
  });

  it("renders one row per model with exact ticking counts", () => {
    const root = container();
    const summary = mockSummary("today");
    renderTokenTicker(root, summary);
    const rows = root.querySelectorAll(".ticker-row");
    expect(rows.length).toBe(summary.byModel.length);
    // First model's exact count is shown with separators.
    const first = root.querySelector<HTMLElement>(".ticker-count");
    expect(first?.textContent).toBe(summary.byModel[0].tokens.toLocaleString("en-US"));
  });

  it("tolerates null totals and null byModel without throwing", () => {
    const root = container();
    const summary = {
      range: "all",
      totals: null,
      byModel: null,
      byProject: null,
      bySource: null,
      timeseries: null,
    } as unknown as Summary;
    expect(() => renderTokenTicker(root, summary)).not.toThrow();
    expect(root.querySelector("#ticker-total")?.textContent).toBe("0");
    expect(root.querySelector(".ticker-empty")?.textContent).toBe("no model data");
  });

  it("tolerates a fully null summary", () => {
    const root = container();
    expect(() => renderTokenTicker(root, null)).not.toThrow();
    expect(root.querySelector("#ticker-total")?.textContent).toBe("0");
  });
});

describe("updateTokenTicker", () => {
  it("ticks the total up to a new value on live update", () => {
    const root = container();
    const base = mockSummary("today");
    renderTokenTicker(root, base);

    const bumped: Summary = {
      ...base,
      totals: { ...base.totals, tokens: base.totals.tokens + 12_345 },
    };
    updateTokenTicker(root, bumped);
    expect(root.querySelector("#ticker-total")?.textContent).toBe(
      bumped.totals.tokens.toLocaleString("en-US"),
    );
  });

  it("rebuilds rows when the model set changes", () => {
    const root = container();
    const base = mockSummary("today");
    renderTokenTicker(root, base);
    expect(root.querySelectorAll(".ticker-row").length).toBe(base.byModel.length);

    const fewer: Summary = { ...base, byModel: base.byModel.slice(0, 1) };
    updateTokenTicker(root, fewer);
    expect(root.querySelectorAll(".ticker-row").length).toBe(1);
  });

  it("does not throw when updating with null data", () => {
    const root = container();
    renderTokenTicker(root, mockSummary("today"));
    expect(() => updateTokenTicker(root, null)).not.toThrow();
  });
});
