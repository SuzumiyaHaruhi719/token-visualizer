import { describe, it, expect } from "vitest";
import { buildClawdSvg } from "./Clawd";
import { applyState } from "./pet-main";
import type { SessionState } from "../lib/types";

describe("buildClawdSvg", () => {
  it("emits grouped parts as SVG <g> with part classes", () => {
    const svg = buildClawdSvg(document);
    expect(svg.tagName.toLowerCase()).toBe("svg");
    expect(svg.classList.contains("clawd")).toBe(true);
    expect(svg.querySelector("g.p-body")).toBeTruthy();
    expect(svg.querySelector("g.p-eyes")).toBeTruthy();
    expect(svg.querySelector("g.p-arml")).toBeTruthy();
    expect(svg.querySelector("g.p-armr")).toBeTruthy();
    // four leg groups
    for (let i = 1; i <= 4; i++) {
      expect(svg.querySelector(`g.p-leg.p-leg${i}`)).toBeTruthy();
    }
    // crisp pixels + non-empty rects
    expect(svg.getAttribute("shape-rendering")).toBe("crispEdges");
    expect(svg.querySelectorAll("rect").length).toBeGreaterThan(0);
  });
});

describe("applyState wiring", () => {
  function els() {
    return {
      stage: document.createElement("div"),
      bubble: document.createElement("div"),
      toolTag: document.createElement("div"),
      label: document.createElement("div"),
    };
  }
  const base: Omit<SessionState, "state"> = {
    sessionId: "s",
    project: "CorePilot",
    model: "claude-opus-4-8",
    tokens: 1,
    updatedAt: 0,
  };

  it("working shows tool tag + working class", () => {
    const e = els();
    applyState(e as any, { ...base, state: { kind: "working", tool: "Bash" } });
    expect(e.stage.classList.contains("state-working")).toBe(true);
    expect(e.toolTag.hidden).toBe(false);
    expect(e.toolTag.textContent).toBe("Bash");
    expect(e.label.textContent).toBe("CorePilot");
  });

  it("thinking shows a bubble and hides the tool tag", () => {
    const e = els();
    applyState(e as any, { ...base, state: { kind: "thinking" } });
    expect(e.stage.classList.contains("state-thinking")).toBe(true);
    expect(e.bubble.hidden).toBe(false);
    expect(e.bubble.textContent).toBe("💭");
    expect(e.toolTag.hidden).toBe(true);
  });

  it("idle hides the bubble", () => {
    const e = els();
    applyState(e as any, { ...base, state: { kind: "idle" } });
    expect(e.bubble.hidden).toBe(true);
  });

  it("swaps state classes (no stale class left behind)", () => {
    const e = els();
    applyState(e as any, { ...base, state: { kind: "working", tool: "Read" } });
    applyState(e as any, { ...base, state: { kind: "sleeping" } });
    expect(e.stage.classList.contains("state-working")).toBe(false);
    expect(e.stage.classList.contains("state-sleeping")).toBe(true);
  });
});
