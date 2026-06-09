import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// The popover reuses the dashboard odometer (reels) + LiveCreep. Mock nothing
// there — we exercise the real reuse — but we drive the exported render helpers
// directly with fixtures (no network), the way dashboard.test.ts does.
import {
  renderModels,
  renderSessions,
  renderCodex,
  renderHero,
  updateHero,
  applySize,
} from "./popover-main";
import { formatTokens } from "../lib/format";
import type { ByModel, SessionState, Limits } from "../lib/types";

function models(): ByModel[] {
  return [
    { model: "claude-opus-4-8", tokens: 1_200_000, costUsd: 12 },
    { model: "claude-sonnet-4-6", tokens: 400_000, costUsd: 3 },
    { model: "gpt-5.4-codex", tokens: 90_000, costUsd: null },
    { model: "claude-haiku-4-5", tokens: 8_000, costUsd: 0.1 }, // 4th → dropped (TOP_N=3)
  ];
}

function sessions(): SessionState[] {
  const now = Date.now();
  return [
    {
      sessionId: "a",
      project: "claude-monitor",
      model: "claude-opus-4-8",
      state: { kind: "working", tool: "Edit" },
      tokens: 1_200_000,
      updatedAt: now,
      source: "claude",
      lastUserMessage: "add a freshness label to the rows",
    },
    {
      sessionId: "b",
      project: "CorePilot",
      model: "claude-sonnet-4-6",
      state: { kind: "waiting" },
      tokens: 400_000,
      updatedAt: now,
      source: "claude",
      lastUserMessage: "wire up the fan curve",
    },
    {
      sessionId: "c",
      project: "New project",
      model: "gpt-5.4-codex",
      state: { kind: "thinking" },
      tokens: 187_046,
      updatedAt: now,
      source: "codex",
      lastUserMessage: "port the remote-access flow",
    },
  ];
}

function limits(withCodex: boolean): Limits {
  return {
    claude: { session: null, fiveHour: null, weekly: null, note: "" },
    codex: withCodex
      ? {
          session: { model: "gpt-5.4-codex", tokens: 412_000 },
          fiveHour: { usedPercent: 38, remainingPercent: 62, resetsAt: 0 },
          weekly: { usedPercent: 71, remainingPercent: 29, resetsAt: 0 },
          planType: "Plus",
        }
      : { session: null, fiveHour: null, weekly: null, planType: null },
  };
}

beforeEach(() => {
  document.body.innerHTML = `
    <main id="popover">
      <span class="pop-spend" id="pop-spend"></span>
      <span class="pop-hero-num" id="pop-hero-num"></span>
      <span class="pop-hero-suffix" id="pop-hero-suffix" hidden></span>
      <span id="pop-codex-model"></span>
      <span id="pop-codex-tok"></span>
      <div id="pop-codex-windows"></div>
      <ul id="pop-models"></ul>
      <ul id="pop-sessions"></ul>
    </main>`;
});

describe("popover top-models", () => {
  it("renders only the top 3 models by tokens", () => {
    renderModels(document.getElementById("pop-models")!, models());
    const rows = document.querySelectorAll(".pop-model-row");
    expect(rows.length).toBe(3);
    // Highest-usage model is shown first, claude- prefix stripped.
    expect(document.querySelector(".pop-name")?.textContent).toBe("opus-4-8");
  });

  it("shows an empty state with no models", () => {
    renderModels(document.getElementById("pop-models")!, []);
    expect(document.querySelector(".pop-model-empty")).toBeTruthy();
  });
});

describe("popover live sessions", () => {
  it("renders one row per active session with the last user message (not the project)", () => {
    renderSessions(document.getElementById("pop-sessions")!, sessions());
    const rows = document.querySelectorAll(".pop-sess-row");
    expect(rows.length).toBe(3);
    // The last user message is the primary text; the project name is gone.
    expect(document.querySelector(".pop-sess-msg")?.textContent).toBe(
      "add a freshness label to the rows",
    );
    expect(document.querySelector(".pop-sess-proj")).toBeNull();
    // Working (Claude) + thinking (Codex) dots are active; the waiting one is not.
    expect(document.querySelectorAll(".pop-sess-dot.active").length).toBe(2);
    // Each row carries a freshness label.
    expect(document.querySelectorAll(".pop-sess-fresh").length).toBe(3);
  });

  it("renders a Codex live session with a Codex source icon + its state", () => {
    renderSessions(document.getElementById("pop-sessions")!, sessions());
    // The text badge is replaced by an inline source icon, tinted per source.
    expect(document.querySelectorAll(".pop-sess-icon--codex").length).toBe(1);
    expect(document.querySelectorAll(".pop-sess-icon--claude").length).toBe(2);
    // The Codex row shows its live "thinking" state (Q2: not masked).
    const states = [...document.querySelectorAll(".pop-sess-state")].map((n) => n.textContent);
    expect(states.some((s) => s?.includes("thinking"))).toBe(true);
  });

  it("patches tokens + freshness baseline in place when rows are reused (no rebuild)", () => {
    const list = document.getElementById("pop-sessions")!;
    const base = sessions();
    renderSessions(list, base);
    const firstRow = list.querySelector(".pop-sess-row")!;
    const stamp0 = firstRow.querySelector(".pop-sess-fresh")!.getAttribute("data-updated");
    const tok0 = firstRow.querySelector(".pop-sess-tok")!.textContent;

    // Same identity/message/state/model/source (signature unchanged) but newer
    // activity + more tokens — the row must NOT rebuild, yet tokens + the
    // freshness baseline (data-updated) must update so "Ns ago" can't go stale.
    const newStamp = Number(base[0].updatedAt) + 5_000;
    const newTokens = base[0].tokens + 333_000;
    const next = base.map((s, i) =>
      i === 0 ? { ...s, tokens: newTokens, updatedAt: newStamp } : s,
    );
    renderSessions(list, next);

    // Reused (same DOM node), not rebuilt.
    expect(list.querySelector(".pop-sess-row")).toBe(firstRow);
    expect(firstRow.querySelector(".pop-sess-fresh")!.getAttribute("data-updated")).toBe(
      String(newStamp),
    );
    expect(firstRow.querySelector(".pop-sess-fresh")!.getAttribute("data-updated")).not.toBe(
      stamp0,
    );
    // Token text changed to the new formatted value (not frozen at the old one).
    const tok1 = firstRow.querySelector(".pop-sess-tok")!.textContent;
    expect(tok1).toBe(`${formatTokens(newTokens)} tok`);
    expect(tok1).not.toBe(tok0);
  });

  it("shows an empty state with no sessions", () => {
    renderSessions(document.getElementById("pop-sessions")!, []);
    expect(document.querySelector(".pop-sess-empty")).toBeTruthy();
  });
});

describe("popover Codex session", () => {
  it("renders the Codex model + tokens + rate windows (never the Claude session)", () => {
    renderCodex(limits(true));
    expect(document.getElementById("pop-codex-model")?.textContent).toBe("gpt-5.4-codex");
    expect(document.getElementById("pop-codex-tok")?.textContent).toMatch(/tok$/);
    const windows = document.querySelectorAll("#pop-codex-windows .pop-win");
    expect(windows.length).toBe(2); // 5h + weekly
  });

  it("shows an empty note when there is no Codex session", () => {
    renderCodex(limits(false));
    expect(document.getElementById("pop-codex-model")?.textContent).toMatch(/No active/i);
    expect(document.getElementById("pop-codex-windows")?.innerHTML).toBe("");
  });
});

describe("popover hero reel reuse", () => {
  it("mounts the reused reels odometer in wide mode (snap)", () => {
    // Wide: reset the precision budget to full via a wide measurement.
    applySize(document.getElementById("popover")!, 320, 400, 1_523_456_789);
    renderHero(1_523_456_789, "snap");
    // The reused odometer renders a .odometer-reels element into the hero host.
    const reel = document.querySelector("#pop-hero-num .odometer-reels");
    expect(reel).toBeTruthy();
    // aria-label carries the full integer value (odometer's syncLabel).
    expect(reel?.getAttribute("aria-label")).toBe("1,523,456,789");
    // No unit suffix in full mode.
    expect(document.getElementById("pop-hero-suffix")?.hidden).toBe(true);
  });

  it("steps to a scaled mantissa + suffix when the width is narrow (graduated precision)", () => {
    const root = document.getElementById("popover")!;
    // Narrow width → ~3 significant digits → 147,881,389 shows as "148" + "M".
    applySize(root, 250, 300, 147_881_389);
    renderHero(147_881_389, "snap");
    const reel = document.querySelector("#pop-hero-num .odometer-reels");
    expect(reel?.getAttribute("aria-label")).toBe("148");
    expect(document.getElementById("pop-hero-suffix")?.hidden).toBe(false);
    expect(document.getElementById("pop-hero-suffix")?.textContent).toBe("M");
    // Reset to wide so later tests (module-shared state) start from full.
    applySize(root, 320, 400, 147_881_389);
  });

  it("rolls a B-scale value cleanly at the tightest width", () => {
    const root = document.getElementById("popover")!;
    applySize(root, 230, 300, 1_523_456_789); // tightest → 2 sig digits
    renderHero(1_523_456_789, "snap");
    // 1,523,456,789 at 2 digits → "1.5B"-worth, reel shows the integer mantissa "2" (round) with "B".
    const reel = document.querySelector("#pop-hero-num .odometer-reels");
    expect(document.getElementById("pop-hero-suffix")?.textContent).toBe("B");
    // Mantissa is a small integer (1 or 2 after rounding 1.52→2), never the full number.
    const label = reel?.getAttribute("aria-label") ?? "";
    expect(label.length).toBeLessThanOrEqual(2);
    applySize(root, 320, 400, 1_523_456_789);
  });

  it("snaps DOWN on a local-midnight rollover instead of freezing (raise-only guard)", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 5, 8, 23, 59)); // late on day 1
    const root = document.getElementById("popover")!;
    applySize(root, 320, 400, 5_000_000); // full integer mode
    // Establish a healthy "today" total (first paint snaps up + records the date).
    updateHero(5_000_000, true);
    expect(
      document.querySelector("#pop-hero-num .odometer-reels")?.getAttribute("aria-label"),
    ).toBe("5,000,000");
    // Cross local midnight: "today" resets to a tiny value on the NEXT day. A
    // raise-only reel would stay at 5,000,000; the date-key rollover guard must
    // snap it down to the new day's total.
    vi.setSystemTime(new Date(2026, 5, 9, 0, 1)); // just after midnight → new day
    updateHero(12_000, false);
    expect(
      document.querySelector("#pop-hero-num .odometer-reels")?.getAttribute("aria-label"),
    ).toBe("12,000");
    vi.useRealTimers();
  });
});

afterEach(() => {
  vi.useRealTimers();
});
