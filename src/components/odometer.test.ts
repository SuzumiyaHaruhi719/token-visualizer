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

  it("snaps immediately on a downward target (range switch)", () => {
    const odo = createOdometer();
    odo.snapTo(900);
    odo.setTarget(200);
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
});
