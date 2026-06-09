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
    // Cost now renders in the chosen billing currency; USD uses the "US$" symbol.
    expect(document.getElementById("kpi-cost")?.textContent).toMatch(/^US\$/);
    expect(document.getElementById("kpi-cache")?.textContent).toMatch(/%$/);
  });

  it("defers the chart morph on a range switch until the odometer roll settles", async () => {
    vi.useFakeTimers();
    try {
      await bootstrap();
      setOption.mockClear();
      await loadRange("7d");
      // Beat 1: the numbers transition immediately, but the charts are held back
      // so no ECharts rAF competes with the 800ms token-odometer roll — so NO
      // chart setOption has fired yet right after the switch.
      expect(setOption).not.toHaveBeenCalled();

      // Mid-roll (before the morph fires): even though a live SSE `usage` refresh
      // lands in this window (the mock stream emits one), the charts must NOT be
      // repainted — a live refresh updates numbers only while a switch is pending,
      // so it can't fight the odometer roll OR pre-empt the deferred morph.
      await vi.advanceTimersByTimeAsync(500);
      expect(setOption).not.toHaveBeenCalled();

      // Beat 2: once the roll settles the three charts MORPH to the new range —
      // each calls setOption with the UPDATE tween enabled (animation on,
      // animationDurationUpdate set), the opposite of the old animation:false snap.
      await vi.advanceTimersByTimeAsync(800);
      const animated = setOption.mock.calls
        .map(([option]) => option as { animation?: boolean; animationDurationUpdate?: number })
        .filter((o) => o.animation === true);
      // The morph fired: all three charts repainted with the update tween enabled.
      expect(animated.length).toBeGreaterThanOrEqual(3);
      for (const option of animated) {
        expect(option.animationDurationUpdate).toBeGreaterThan(0);
      }
      // Every chart paint that occurs is an animated morph (never a hard
      // animation:false snap) — the morph and any post-morph live refresh both
      // carry an update tween.
      for (const [option] of setOption.mock.calls) {
        const o = option as { animation?: boolean; animationDurationUpdate?: number };
        expect(o.animation).toBe(true);
        expect(o.animationDurationUpdate).toBeGreaterThan(0);
      }
    } finally {
      vi.useRealTimers();
    }
  });

  it("marks the active range tab", async () => {
    await bootstrap();
    await loadRange("30d");
    const active = document.querySelector(".range-tab.active") as HTMLElement;
    expect(active?.dataset.range).toBe("30d");
  });

  it("renders the current-session strip with the last user message (not the project)", async () => {
    await bootstrap();
    renderCurrent({
      ...mockSessions()[0],
      lastUserMessage: "fix the failing build",
    });
    // The last user message is the primary text; the project name is gone.
    expect(document.querySelector(".cs-msg")?.textContent).toBe("fix the failing build");
    expect(document.querySelector(".cs-project")).toBeNull();
    expect(document.querySelector(".cs-tokens")?.textContent).toMatch(/tok$/);
    // A source icon + a freshness label render alongside.
    expect(document.querySelector(".cs-source-icon")).toBeTruthy();
    expect(document.querySelector(".cs-freshness")?.textContent).toBeTruthy();
  });

  it("renders a DeepSeek current session with the DeepSeek source glyph", async () => {
    await bootstrap();
    renderCurrent({
      ...mockSessions()[0],
      source: "deepseek",
      model: "deepseek-v4-pro",
      lastUserMessage: "帮我部署这个仓库",
    });
    // The source flow reaches the live strip: the glyph carries the deepseek
    // modifier (its accent), not the default Claude one.
    expect(document.querySelector(".cs-source-icon--deepseek")).toBeTruthy();
    expect(document.querySelector(".cs-source-icon--claude")).toBeNull();
    expect(document.querySelector(".cs-msg")?.textContent).toBe("帮我部署这个仓库");
  });

  it("shows an empty state when there is no current session", async () => {
    await bootstrap();
    renderCurrent(null);
    expect(document.querySelector(".cs-empty")).toBeTruthy();
  });

  it("mounts a subtle manual refresh control in the limits panel header", async () => {
    await bootstrap();
    const btn = document.getElementById("limits-refresh");
    expect(btn).toBeTruthy();
    // Unobtrusive: just the circular glyph, no text label.
    expect(btn?.textContent?.trim()).toBe("⟳");
    expect(btn?.getAttribute("aria-label")).toBe("Refresh limits");
    // Clicking triggers a refresh without throwing (mock-backed).
    (btn as HTMLButtonElement).click();
    await Promise.resolve();
    expect(document.querySelector(".limit-card-codex")).toBeTruthy();
  });
});
