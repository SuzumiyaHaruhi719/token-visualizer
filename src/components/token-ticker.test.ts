import { describe, it, expect } from "vitest";
import { renderTokenTicker, updateTokenTicker } from "./token-ticker";
import { mockSummary } from "../lib/api";
import type { Summary } from "../lib/types";

function container(): HTMLElement {
  return document.createElement("div");
}

/** Read an odometer's displayed value via its data-value (the rolled digits
 *  live in stacked glyph strips, so textContent isn't the displayed number). */
function odoValue(el: Element | null): string | undefined {
  return el?.querySelector<HTMLElement>(".odometer")?.dataset.value;
}

describe("renderTokenTicker", () => {
  it("renders the exact total with thousands separators", () => {
    const root = container();
    const summary = mockSummary("today");
    renderTokenTicker(root, summary);
    expect(odoValue(root.querySelector("#ticker-total"))).toBe(
      summary.totals.tokens.toLocaleString("en-US"),
    );
  });

  it("renders one row per model with exact rolling counts", () => {
    const root = container();
    const summary = mockSummary("today");
    renderTokenTicker(root, summary);
    const rows = root.querySelectorAll(".ticker-row");
    expect(rows.length).toBe(summary.byModel.length);
    // First model's exact count is shown with separators.
    const first = root.querySelector<HTMLElement>(".ticker-count");
    expect(odoValue(first)).toBe(summary.byModel[0].tokens.toLocaleString("en-US"));
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
    expect(odoValue(root.querySelector("#ticker-total"))).toBe("0");
    expect(root.querySelector(".ticker-empty")?.textContent).toBe("no model data");
  });

  it("tolerates a fully null summary", () => {
    const root = container();
    expect(() => renderTokenTicker(root, null)).not.toThrow();
    expect(odoValue(root.querySelector("#ticker-total"))).toBe("0");
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
    expect(odoValue(root.querySelector("#ticker-total"))).toBe(
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
