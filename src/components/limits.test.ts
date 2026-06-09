import { describe, it, expect } from "vitest";
import { renderLimits, tickCountdowns, formatCountdown } from "./limits";
import { renderBySource } from "./by-source";
import { mockLimits } from "../lib/api";
import type { Limits } from "../lib/types";

function container(): HTMLElement {
  return document.createElement("div");
}

describe("formatCountdown", () => {
  it("formats hours and minutes", () => {
    const now = 1_000_000_000_000; // fixed nowMs
    const resetsAt = now / 1000 + 2 * 3600 + 14 * 60;
    expect(formatCountdown(resetsAt, now)).toBe("2h 14m");
  });

  it("formats minutes and seconds when under an hour", () => {
    const now = 1_000_000_000_000;
    const resetsAt = now / 1000 + 5 * 60 + 9;
    expect(formatCountdown(resetsAt, now)).toBe("5m 9s");
  });

  it("returns 'now' when the window has elapsed", () => {
    const now = 1_000_000_000_000;
    expect(formatCountdown(now / 1000 - 10, now)).toBe("now");
  });
});

describe("renderLimits", () => {
  it("renders Claude session + note and no fake gauges", () => {
    const root = container();
    renderLimits(root, mockLimits());
    expect(root.querySelector(".limit-card-claude")).toBeTruthy();
    expect(root.querySelector(".limit-card-claude .limit-proj")?.textContent).toBe("claude-monitor");
    expect(root.querySelector(".limit-card-claude .limit-note")?.textContent).toMatch(/remaining/);
    // Claude card has no gauge bars.
    expect(root.querySelector(".limit-card-claude .limit-bar")).toBeNull();
  });

  it("renders Codex 5h + Weekly gauges with countdowns and a plan chip", () => {
    const root = container();
    renderLimits(root, mockLimits());
    const codex = root.querySelector(".limit-card-codex")!;
    const gauges = codex.querySelectorAll(".limit-gauge[data-resets-at]");
    expect(gauges.length).toBe(2);
    expect(codex.querySelector(".limit-chip")?.textContent).toBe("Plus");
    expect(codex.querySelectorAll(".limit-countdown").length).toBe(2);
  });

  it("shows graceful empty states when sources are absent", () => {
    const empty: Limits = {
      claude: { session: null, fiveHour: null, weekly: null, note: "remaining not exposed locally" },
      codex: { session: null, fiveHour: null, weekly: null, planType: null },
    };
    const root = container();
    renderLimits(root, empty);
    expect(root.querySelectorAll(".limit-empty").length).toBe(2);
    expect(root.querySelector(".limit-chip")).toBeNull();
  });

  it("tickCountdowns refreshes the visible countdown text", () => {
    const root = container();
    renderLimits(root, mockLimits());
    const before = root.querySelector(".limit-countdown")?.textContent;
    // Advance "now" by an hour; the countdown should shrink.
    tickCountdowns(root, Date.now() + 3_600_000);
    const after = root.querySelector(".limit-countdown")?.textContent;
    expect(after).not.toBe(before);
  });
});

describe("renderBySource", () => {
  it("renders a split bar and per-source chips for all three sources", () => {
    const root = container();
    renderBySource(root, [
      { source: "claude", tokens: 80, costUsd: 1 },
      { source: "codex", tokens: 10, costUsd: 0.1 },
      { source: "deepseek", tokens: 10, costUsd: 0.05 },
    ]);
    // Claude / Codex / DeepSeek each get a segment + chip.
    expect(root.querySelectorAll(".bysource-seg").length).toBe(3);
    expect(root.querySelector(".bysource-seg.bysource-deepseek")).toBeTruthy();
    const chips = Array.from(root.querySelectorAll(".bysource-chip")).map((c) => c.textContent);
    expect(chips.join(" ")).toMatch(/Claude 80%/);
    expect(chips.join(" ")).toMatch(/Codex 10%/);
    expect(chips.join(" ")).toMatch(/DeepSeek 10%/);
  });

  it("shows an empty state when there is no source data", () => {
    const root = container();
    renderBySource(root, []);
    expect(root.querySelector(".bysource-empty")).toBeTruthy();
  });

  it("stages the final segment widths on fresh split markup", () => {
    // The global test-setup runs rAF synchronously, so the deferred width
    // staging (0% -> final) settles to the final width within this call.
    const root = container();
    renderBySource(root, [
      { source: "claude", tokens: 75, costUsd: 1 },
      { source: "codex", tokens: 25, costUsd: 0.3 },
    ]);
    const claude = root.querySelector<HTMLElement>('.bysource-seg[data-source="claude"]');
    const codex = root.querySelector<HTMLElement>('.bysource-seg[data-source="codex"]');
    // jsdom canonicalizes "75.00%" -> "75%"; assert the percentage, not formatting.
    expect(parseFloat(claude?.style.width ?? "")).toBeCloseTo(75, 2);
    expect(parseFloat(codex?.style.width ?? "")).toBeCloseTo(25, 2);
  });
});

describe("renderLimits gauge fills", () => {
  it("paints Codex gauge fills at their used-percent width (no grow-from-0)", () => {
    // The fill renders at its FINAL width directly. Animating 0->used on every
    // refresh made the gauge look like it was constantly auto-refreshing, so the
    // bar now paints statically at its value and only moves when the % changes.
    const root = container();
    renderLimits(root, mockLimits());
    const fills = root.querySelectorAll<HTMLElement>(".limit-card-codex .limit-bar-fill");
    expect(fills.length).toBe(2);
    fills.forEach((fill) => {
      // A real, non-zero width (not the old 0% start state).
      const width = parseFloat(fill.style.width);
      expect(width).toBeGreaterThan(0);
      expect(width).toBeLessThanOrEqual(100);
    });
  });
});
