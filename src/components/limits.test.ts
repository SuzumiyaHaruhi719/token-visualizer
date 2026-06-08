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
  it("renders a split bar and per-source chips", () => {
    const root = container();
    renderBySource(root, [
      { source: "claude", tokens: 90, costUsd: 1 },
      { source: "codex", tokens: 10, costUsd: 0.1 },
    ]);
    expect(root.querySelectorAll(".bysource-seg").length).toBe(2);
    const chips = Array.from(root.querySelectorAll(".bysource-chip")).map((c) => c.textContent);
    expect(chips.join(" ")).toMatch(/Claude 90%/);
    expect(chips.join(" ")).toMatch(/Codex 10%/);
  });

  it("shows an empty state when there is no source data", () => {
    const root = container();
    renderBySource(root, []);
    expect(root.querySelector(".bysource-empty")).toBeTruthy();
  });
});
