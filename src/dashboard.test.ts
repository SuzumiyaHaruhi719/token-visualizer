import { describe, it, expect, beforeEach, vi } from "vitest";

// Mock ECharts so we exercise the dashboard's DOM + data wiring in jsdom
// without zrender's real canvas painting (jsdom has no 2D canvas).
const setOption = vi.fn();
vi.mock("echarts", () => ({
  init: () => ({ setOption, resize: () => {}, dispose: () => {} }),
  graphic: {
    LinearGradient: class {
      constructor(
        public x: number,
        public y: number,
        public x2: number,
        public y2: number,
        public colorStops: { offset: number; color: string }[],
      ) {}
    },
  },
}));

import { bootstrap, loadRange, renderCurrent } from "./main";
import { mockSessions } from "./lib/api";

beforeEach(() => {
  setOption.mockClear();
  document.body.innerHTML = `<main id="app"></main>`;
  (window as any).__CM_MOCK__ = true;
  // Snap number tweens instantly so KPI text is final right after render.
  (window as any).matchMedia = (q: string) => ({
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
    // The chart redraw is deferred/debounced (so it doesn't hitch the number
    // roll), so wait for that timer before asserting.
    await new Promise((r) => setTimeout(r, 220));
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
