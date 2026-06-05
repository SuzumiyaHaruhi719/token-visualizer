import { describe, it, expect, beforeEach, vi } from "vitest";

// Mock ECharts so we exercise the dashboard's DOM + data wiring in jsdom
// without zrender's real canvas painting (jsdom has no 2D canvas).
const setOption = vi.fn();
vi.mock("echarts", () => ({
  init: () => ({ setOption, resize: () => {}, dispose: () => {} }),
}));

import { bootstrap, loadRange, renderCurrent } from "./main";
import { mockSessions } from "./lib/api";

beforeEach(() => {
  setOption.mockClear();
  document.body.innerHTML = `<main id="app"></main>`;
  (window as any).__CM_MOCK__ = true;
});

describe("dashboard integration (mock data)", () => {
  it("renders KPI values from the mock summary", async () => {
    await bootstrap();
    expect(document.getElementById("kpi-tokens")?.textContent).toMatch(/M$/);
    expect(document.getElementById("kpi-cost")?.textContent).toMatch(/^\$/);
    expect(document.getElementById("kpi-cache")?.textContent).toMatch(/%$/);
  });

  it("configures all three charts on range load", async () => {
    await bootstrap();
    setOption.mockClear();
    await loadRange("7d");
    // timeseries + donut + projects each call setOption at least once
    expect(setOption.mock.calls.length).toBeGreaterThanOrEqual(3);
  });

  it("marks the active range tab", async () => {
    await bootstrap();
    await loadRange("30d");
    const active = document.querySelector(".range-tab.active") as HTMLElement;
    expect(active?.dataset.range).toBe("30d");
  });

  it("renders the current-session strip from a SessionState", async () => {
    await bootstrap();
    renderCurrent(mockSessions()[0]);
    expect(document.querySelector(".cs-project")?.textContent).toBe("claude-monitor");
    expect(document.querySelector(".cs-tokens")?.textContent).toMatch(/tok$/);
  });

  it("shows an empty state when there is no current session", async () => {
    await bootstrap();
    renderCurrent(null);
    expect(document.querySelector(".cs-empty")).toBeTruthy();
  });
});
