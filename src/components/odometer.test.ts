import { describe, it, expect } from "vitest";
import { createOdometer } from "./odometer";

// The global rAF stub in src/test-setup.ts advances a fake clock by a huge step
// each frame, so the interpolation-buffer driver drains to the latest sample and
// stops scheduling within a couple frames (otherwise the synchronous stub would
// loop forever). We therefore assert on the SETTLED value (textContent /
// data-value), not on intermediate motion.

describe("createOdometer — rendering", () => {
  it("renders one cohesive number with thousands separators", () => {
    const odo = createOdometer();
    odo.snapTo(1234567);
    expect(odo.el.textContent).toBe("1,234,567");
    expect(odo.el.dataset.value).toBe("1,234,567");
    expect(odo.el.getAttribute("aria-label")).toBe("1,234,567");
  });

  it("renders 0 as a single zero", () => {
    const odo = createOdometer();
    odo.snapTo(0);
    expect(odo.el.textContent).toBe("0");
    expect(odo.value()).toBe(0);
  });

  it("has no per-digit wheel/cell sub-structure (cohesive text)", () => {
    const odo = createOdometer();
    odo.snapTo(12345);
    expect(odo.el.querySelectorAll(".odo-digit, .odo-strip, .odo-glyph").length).toBe(0);
  });
});

describe("createOdometer — defensive inputs", () => {
  it("clamps negative values to 0", () => {
    const odo = createOdometer();
    odo.snapTo(-500);
    expect(odo.el.dataset.value).toBe("0");
    odo.setTarget(-1);
    expect(odo.el.dataset.value).toBe("0");
  });

  it("treats non-finite values as 0", () => {
    const odo = createOdometer();
    odo.setTarget(Number.NaN);
    expect(odo.el.dataset.value).toBe("0");
    odo.snapTo(Number.POSITIVE_INFINITY);
    expect(odo.el.dataset.value).toBe("0");
  });

  it("rounds fractional values to the nearest integer", () => {
    const odo = createOdometer();
    odo.snapTo(1234.6);
    expect(odo.el.dataset.value).toBe("1,235");
  });
});

describe("createOdometer — continuous driver", () => {
  it("rolls toward an upward target and settles on the exact value", () => {
    const odo = createOdometer();
    odo.snapTo(100);
    odo.setTarget(305);
    expect(odo.el.dataset.value).toBe("305");
    expect(odo.value()).toBe(305);
  });

  it("ignores a downward setTarget (raise-only live roll), but transitionTo goes down", () => {
    const odo = createOdometer();
    odo.snapTo(900);
    odo.setTarget(200); // raise-only: a lower live value is ignored, not snapped
    expect(odo.el.dataset.value).toBe("900");
    odo.transitionTo(200); // tab switch handles downward
    expect(odo.el.dataset.value).toBe("200");
    expect(odo.value()).toBe(200);
  });

  it("settles on the latest value when setTarget is called repeatedly", () => {
    const odo = createOdometer();
    odo.snapTo(100);
    odo.setTarget(200);
    odo.setTarget(305000);
    expect(odo.el.dataset.value).toBe("305,000");
    expect(odo.value()).toBe(305000);
  });

  it("setValue is an alias for snapTo (immediate set)", () => {
    const odo = createOdometer();
    odo.setValue(54321);
    expect(odo.el.dataset.value).toBe("54,321");
    expect(odo.value()).toBe(54321);
  });

  it("transitionTo rolls to the new value in BOTH directions (tab switch)", () => {
    const odo = createOdometer();
    odo.snapTo(1000);
    odo.transitionTo(5_000_000); // up
    expect(odo.el.dataset.value).toBe("5,000,000");
    odo.transitionTo(200); // down — transitions, does not stay stuck
    expect(odo.el.dataset.value).toBe("200");
    expect(odo.value()).toBe(200);
  });
});

describe("createOdometer — reels (slot-machine) mode", () => {
  it("renders per-digit reel cells + comma separators with the value exposed", () => {
    const odo = createOdometer({ reels: true });
    odo.snapTo(1234567);
    expect(odo.el.classList.contains("odometer-reels")).toBe(true);
    expect(odo.el.querySelectorAll(".odo-digit").length).toBe(7);
    expect(odo.el.querySelectorAll(".odo-sep").length).toBe(2);
    expect(odo.el.dataset.value).toBe("1,234,567");
  });

  it("rolls to an upward target and settles exactly (reels)", () => {
    const odo = createOdometer({ reels: true });
    odo.snapTo(100);
    odo.setTarget(987654);
    expect(odo.el.dataset.value).toBe("987,654");
    expect(odo.value()).toBe(987654);
  });

  it("rolls through many smooth, gradually-increasing frames (not a snap)", () => {
    // Drive real ~16ms frames (instead of the global huge-step stub) to observe
    // the in-flight motion: a single update must roll gradually over many frames,
    // strictly increasing, without jumping straight to the target.
    const real = globalThis.requestAnimationFrame;
    const queue: FrameRequestCallback[] = [];
    globalThis.requestAnimationFrame = ((fn: FrameRequestCallback) => {
      queue.push(fn);
      return queue.length;
    }) as typeof globalThis.requestAnimationFrame;
    try {
      const odo = createOdometer();
      odo.snapTo(1_000_000);
      odo.setTarget(2_000_000);

      const frames: number[] = [];
      let t = 0;
      for (let i = 0; i < 120; i++) {
        const cb = queue.shift();
        if (!cb) break;
        t += 16.7;
        cb(t);
        frames.push(Number((odo.el.dataset.value ?? "0").replace(/,/g, "")));
      }

      expect(frames.length).toBeGreaterThan(60); // didn't converge in a few frames
      expect(frames[0]).toBeGreaterThan(1_000_000); // moved off the start
      expect(frames[0]).toBeLessThan(1_300_000); // but did NOT jump near the target
      for (let i = 1; i < frames.length; i++) {
        expect(frames[i]).toBeGreaterThanOrEqual(frames[i - 1]); // monotonic up
      }
      const last = frames[frames.length - 1];
      expect(last).toBeGreaterThan(1_500_000); // made real progress over ~2s
      expect(last).toBeLessThan(2_000_000); // still rolling — continuous, not done
    } finally {
      globalThis.requestAnimationFrame = real;
    }
  });
});
