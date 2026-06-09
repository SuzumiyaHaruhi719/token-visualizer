import { describe, it, expect, afterEach, vi } from "vitest";
import {
  prefersReducedMotion,
  transitionContentSwap,
  revealContent,
  formatFreshness,
  sourceIconSvg,
} from "./motion";

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

/** A connected element (transitionContentSwap ignores detached nodes). */
function mountEl(): HTMLElement {
  const el = document.createElement("div");
  document.body.appendChild(el);
  return el;
}

describe("prefersReducedMotion", () => {
  afterEach(() => {
    delete (window as any).matchMedia;
    document.body.innerHTML = "";
  });

  it("reflects the matchMedia result", () => {
    setReducedMotion(true);
    expect(prefersReducedMotion()).toBe(true);
    setReducedMotion(false);
    expect(prefersReducedMotion()).toBe(false);
  });

  it("is false when matchMedia is unavailable", () => {
    delete (window as any).matchMedia;
    expect(prefersReducedMotion()).toBe(false);
  });
});

describe("transitionContentSwap", () => {
  afterEach(() => {
    delete (window as any).matchMedia;
    document.body.innerHTML = "";
  });

  it("mutates and resolves true under reduced motion (via the fade, drained)", async () => {
    // Reduced motion now ANIMATES (opacity-only) rather than skipping — it runs
    // the full two-phase swap. Drive the timers/frames manually so it's fast +
    // deterministic, then assert the mutation landed and the classes cleaned up.
    setReducedMotion(true);
    const timers: Array<() => void> = [];
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "setTimeout",
      ((cb: () => void) => {
        timers.push(cb);
        return timers.length as unknown as ReturnType<typeof setTimeout>;
      }) as typeof setTimeout,
    );

    try {
      const el = mountEl();
      const mutate = vi.fn(() => {
        el.textContent = "swapped";
      });
      const done = transitionContentSwap([el], mutate);
      while (timers.length) timers.shift()!();
      while (frames.length) frames.shift()!(0);
      while (timers.length) timers.shift()!();

      expect(await done).toBe(true);
      expect(mutate).toHaveBeenCalledTimes(1);
      expect(el.textContent).toBe("swapped");
      // Transient transition classes are stripped once the swap settles.
      expect(el.classList.contains("content-swap-out")).toBe(false);
      expect(el.classList.contains("content-swap-enter")).toBe(false);
      expect(el.classList.contains("is-reduced")).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("mutates immediately (synchronously) when there are no connected targets", async () => {
    setReducedMotion(false);
    const detached = document.createElement("div"); // never appended
    const mutate = vi.fn();
    const ran = await transitionContentSwap([detached, null, undefined], mutate);
    expect(ran).toBe(true);
    expect(mutate).toHaveBeenCalledTimes(1);
  });

  it("skips the mutation when isCurrent() is already false (no connected targets path)", async () => {
    // The synchronous early-return now fires only on the structural fast path
    // (no targets / no rAF), so exercise it with a detached node.
    setReducedMotion(false);
    const detached = document.createElement("div");
    const mutate = vi.fn();
    const ran = await transitionContentSwap([detached], mutate, {
      isCurrent: () => false,
    });
    expect(ran).toBe(false);
    expect(mutate).not.toHaveBeenCalled();
  });

  it("runs the mutation exactly once across many targets", async () => {
    setReducedMotion(true);
    const timers: Array<() => void> = [];
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "setTimeout",
      ((cb: () => void) => {
        timers.push(cb);
        return timers.length as unknown as ReturnType<typeof setTimeout>;
      }) as typeof setTimeout,
    );
    try {
      const els = [mountEl(), mountEl(), mountEl()];
      const mutate = vi.fn();
      const done = transitionContentSwap(els, mutate);
      while (timers.length) timers.shift()!();
      while (frames.length) frames.shift()!(0);
      while (timers.length) timers.shift()!();
      await done;
      expect(mutate).toHaveBeenCalledTimes(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("cleans up its classes when isCurrent() flips false with no newer swap (no stuck fade)", async () => {
    // A newer FETCH (not a newer swap) can abandon this swap via isCurrent. Since
    // no newer swap claims the targets, THIS swap must strip its own transient
    // classes — otherwise the block stays faded + pointer-events:none forever.
    setReducedMotion(false);
    const timers: Array<() => void> = [];
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "setTimeout",
      ((cb: () => void) => {
        timers.push(cb);
        return timers.length as unknown as ReturnType<typeof setTimeout>;
      }) as typeof setTimeout,
    );
    try {
      const el = mountEl();
      const mutate = vi.fn();
      let current = true;
      const done = transitionContentSwap([el], mutate, { isCurrent: () => current });
      // Phase 1 applied the out-class (block is mid-fade).
      expect(el.classList.contains("content-swap-out")).toBe(true);
      expect(el.classList.contains("content-swap-active")).toBe(true);
      // A newer fetch supersedes us before phase-1 timer fires.
      current = false;
      while (timers.length) timers.shift()!();
      while (frames.length) frames.shift()!(0);
      while (timers.length) timers.shift()!();

      expect(await done).toBe(false);
      expect(mutate).not.toHaveBeenCalled(); // abandoned before mutating
      // CRITICAL: no stuck classes — the block is interactive + visible again.
      expect(el.classList.contains("content-swap-out")).toBe(false);
      expect(el.classList.contains("content-swap-active")).toBe(false);
      expect(el.classList.contains("is-reduced")).toBe(false);
      expect(el.style.getPropertyValue("--swap-stagger")).toBe("");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("under reduced motion runs an opacity-only swap (is-reduced) and still mutates", async () => {
    setReducedMotion(true); // OS animations off → opacity-only, NOT skipped
    const timers: Array<() => void> = [];
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "setTimeout",
      ((cb: () => void) => {
        timers.push(cb);
        return timers.length as unknown as ReturnType<typeof setTimeout>;
      }) as typeof setTimeout,
    );

    try {
      const el = mountEl();
      const mutate = vi.fn();
      const done = transitionContentSwap([el], mutate);

      // Phase 1 applied the opacity-only reduced variant (still visible).
      expect(el.classList.contains("content-swap-out")).toBe(true);
      expect(el.classList.contains("is-reduced")).toBe(true);

      // Drain phase-1 timer → mutate + prime enter; frames → reveal; timers → cleanup.
      while (timers.length) timers.shift()!();
      while (frames.length) frames.shift()!(0);
      while (timers.length) timers.shift()!();

      expect(await done).toBe(true);
      expect(mutate).toHaveBeenCalledTimes(1);
      // Cleanup strips the transient classes.
      expect(el.classList.contains("is-reduced")).toBe(false);
      expect(el.classList.contains("content-swap-active")).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("a superseding swap cancels the older swap's mutation (generation guard)", async () => {
    setReducedMotion(false);
    // Manual timer/rAF queues so we can interleave two overlapping swaps on the
    // SAME element deterministically.
    const timers: Array<() => void> = [];
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    vi.stubGlobal(
      "setTimeout",
      ((cb: () => void) => {
        timers.push(cb);
        return timers.length as unknown as ReturnType<typeof setTimeout>;
      }) as typeof setTimeout,
    );

    try {
      const el = mountEl();
      const firstMutate = vi.fn();
      const secondMutate = vi.fn();

      // Start swap A (it adds out-class and queues its phase-1 timer).
      const aDone = transitionContentSwap([el], firstMutate);
      // Start swap B on the same element BEFORE A's phase-1 timer fires — B bumps
      // the generation, so A must abandon without mutating.
      const bDone = transitionContentSwap([el], secondMutate);

      // Drain phase-1 timers for both.
      while (timers.length) timers.shift()!();
      // Drain frames (rAF callbacks take a timestamp) + follow-up cleanup timers.
      while (frames.length) frames.shift()!(0);
      while (timers.length) timers.shift()!();

      await Promise.all([aDone, bDone]);

      // A's mutation was cancelled by B's generation bump; B's ran.
      expect(firstMutate).not.toHaveBeenCalled();
      expect(secondMutate).toHaveBeenCalledTimes(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("revealContent", () => {
  afterEach(() => {
    delete (window as any).matchMedia;
    document.body.innerHTML = "";
  });

  it("still runs (opacity-only) under reduced motion so the entrance stays visible", () => {
    // Reduced motion no longer no-ops: it runs the gentle opacity-only variant so
    // a Windows "animations off" user still sees the blocks ease in. With the
    // synchronous test rAF the enter class is added then removed within the call.
    setReducedMotion(true);
    const el = mountEl();
    const ran = revealContent([el]);
    expect(ran).toBe(true);
    expect(el.classList.contains("content-swap-enter")).toBe(false);
  });

  it("primes then clears the enter class with motion enabled", () => {
    setReducedMotion(false);
    // Global test-setup stubs rAF to run synchronously, so the enter class is
    // added then removed within this call — the net effect is no leftover class.
    const el = mountEl();
    const ran = revealContent([el]);
    expect(ran).toBe(true);
    expect(el.classList.contains("content-swap-enter")).toBe(false);
  });

  it("returns false when there are no connected targets", () => {
    setReducedMotion(false);
    expect(revealContent([null, undefined])).toBe(false);
  });
});

describe("formatFreshness", () => {
  const now = 1_000_000_000_000; // fixed reference clock

  it("reads as 'just now' under 10 seconds", () => {
    expect(formatFreshness(now - 0, now)).toBe("just now");
    expect(formatFreshness(now - 9_000, now)).toBe("just now");
  });

  it("shows whole seconds between 10s and a minute", () => {
    expect(formatFreshness(now - 10_000, now)).toBe("10s ago");
    expect(formatFreshness(now - 59_000, now)).toBe("59s ago");
  });

  it("shows whole minutes under an hour", () => {
    expect(formatFreshness(now - 60_000, now)).toBe("1m ago");
    expect(formatFreshness(now - 59 * 60_000, now)).toBe("59m ago");
  });

  it("shows whole hours under a day, then days", () => {
    expect(formatFreshness(now - 3_600_000, now)).toBe("1h ago");
    expect(formatFreshness(now - 23 * 3_600_000, now)).toBe("23h ago");
    expect(formatFreshness(now - 2 * 86_400_000, now)).toBe("2d ago");
  });

  it("treats non-finite / missing / future stamps as 'just now'", () => {
    expect(formatFreshness(undefined, now)).toBe("just now");
    expect(formatFreshness(null, now)).toBe("just now");
    expect(formatFreshness(Number.NaN, now)).toBe("just now");
    expect(formatFreshness(now + 5_000, now)).toBe("just now"); // small clock skew
  });
});

describe("sourceIconSvg", () => {
  it("emits a monochrome inline SVG tinted via a source modifier class", () => {
    const claude = sourceIconSvg("claude", "cs-source-icon");
    expect(claude).toContain("<svg");
    expect(claude).toContain("cs-source-icon cs-source-icon--claude");
    expect(claude).toContain('stroke="currentColor"');
    expect(claude).toContain('aria-hidden="true"');

    const codex = sourceIconSvg("codex", "pop-sess-icon");
    expect(codex).toContain("pop-sess-icon pop-sess-icon--codex");
    // The two sources use distinct path data.
    expect(claude).not.toBe(codex);
  });

  it("emits a distinct DeepSeek glyph tinted via its modifier class", () => {
    const deepseek = sourceIconSvg("deepseek", "cs-source-icon");
    expect(deepseek).toContain("<svg");
    expect(deepseek).toContain("cs-source-icon cs-source-icon--deepseek");
    expect(deepseek).toContain('stroke="currentColor"');
    // Its path data differs from both other sources.
    const claude = sourceIconSvg("claude", "cs-source-icon");
    const codex = sourceIconSvg("codex", "cs-source-icon");
    expect(deepseek).not.toBe(claude);
    expect(deepseek).not.toBe(codex);
  });
});
