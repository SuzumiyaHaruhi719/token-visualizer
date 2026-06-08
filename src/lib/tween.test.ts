import { describe, it, expect, afterEach, vi } from "vitest";
import { animateNumber } from "./tween";

function makeEl(): HTMLElement {
  return document.createElement("div");
}

function setReducedMotion(reduce: boolean): void {
  (window as any).matchMedia = (q: string) => ({
    matches: reduce,
    media: q,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    onchange: null,
    dispatchEvent: () => false,
  });
}

describe("animateNumber", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    delete (window as any).matchMedia;
  });

  it("snaps to the final formatted value under reduced motion", () => {
    setReducedMotion(true);
    const el = makeEl();
    animateNumber(el, 0, 1_000_000, { format: (v) => `${Math.round(v)}` });
    expect(el.textContent).toBe("1000000");
  });

  it("snaps when from === to without scheduling a frame", () => {
    setReducedMotion(false);
    const el = makeEl();
    animateNumber(el, 42, 42, { format: (v) => `v${v}` });
    expect(el.textContent).toBe("v42");
  });

  it("uses the provided formatter for the final value", () => {
    setReducedMotion(true);
    const el = makeEl();
    animateNumber(el, 0, 0.5, { format: (v) => `${Math.round(v * 100)}%` });
    expect(el.textContent).toBe("50%");
  });

  it("tweens through intermediate values and settles on the target", () => {
    setReducedMotion(false);
    // Deterministic rAF: advance time and run callbacks synchronously.
    let now = 0;
    const queue: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      queue.push(cb);
      return queue.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});

    const el = makeEl();
    const seen: string[] = [];
    animateNumber(el, 0, 100, {
      durationMs: 100,
      format: (v) => {
        const s = `${Math.round(v)}`;
        seen.push(s);
        return s;
      },
    });

    // Drive frames until the queue drains.
    for (let i = 0; i < 50 && queue.length > 0; i++) {
      const cb = queue.shift()!;
      now += 25;
      cb(now);
    }

    expect(el.textContent).toBe("100");
    expect(seen.length).toBeGreaterThan(1);
  });
});
