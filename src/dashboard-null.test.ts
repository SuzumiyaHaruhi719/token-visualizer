import { describe, it, expect, beforeEach, vi } from "vitest";

// Mock ECharts (jsdom has no real canvas), like dashboard.test.ts.
const setOption = vi.fn();
vi.mock("echarts", () => ({
  init: () => ({ setOption, resize: () => {}, dispose: () => {} }),
}));

// Feed deliberately null / partial API payloads to exercise main.ts's
// summary normalization (the hardening that lets KPIs + charts tolerate
// missing data instead of throwing).
vi.mock("./lib/api", () => ({
  getSummary: vi.fn(async () => ({
    range: "all",
    totals: null,
    byModel: null,
    byProject: null,
    timeseries: null,
  })),
  getCurrent: vi.fn(async () => null),
  subscribe: vi.fn(() => () => {}),
}));

import { bootstrap, loadRange, renderCurrent } from "./main";

beforeEach(() => {
  setOption.mockClear();
  document.body.innerHTML = `<main id="app"></main>`;
});

describe("dashboard resilience (null / partial data)", () => {
  it("bootstraps without throwing and shows placeholder KPIs", async () => {
    await bootstrap(); // would throw if summary normalization were missing
    expect(document.getElementById("kpi-tokens")?.textContent).toBeTruthy();
    expect(document.getElementById("kpi-cost")?.textContent).toBeTruthy();
    expect(document.getElementById("kpi-cache")?.textContent).toBeTruthy();
    expect(document.getElementById("kpi-sessions")?.textContent).toBeTruthy();
  });

  it("loadRange tolerates null payloads and still switches range", async () => {
    await bootstrap();
    await loadRange("30d");
    expect(
      document.querySelector(".range-tab.active")?.getAttribute("data-range"),
    ).toBe("30d");
  });

  it("renderCurrent(null) shows the empty state", async () => {
    await bootstrap();
    renderCurrent(null);
    expect(document.querySelector(".cs-empty")).toBeTruthy();
  });
});
