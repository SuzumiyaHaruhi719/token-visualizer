import { describe, it, expect } from "vitest";
import { createOdometer } from "./odometer";

// The global rAF stub in src/test-setup.ts advances a fake clock by a huge
// step each frame, so any roll settles to its target within a couple frames.
// We therefore assert on the SETTLED DOM (digit count, separators, transforms),
// not on intermediate motion.

function digitCells(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(".odo-digit"));
}

function seps(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(".odo-sep"));
}

/** The settled glyph shown by a digit cell, derived from its strip transform. */
function shownDigit(cell: HTMLElement): number {
  const strip = cell.querySelector<HTMLElement>(".odo-strip")!;
  const m = /translateY\(-([\d.]+)%\)/.exec(strip.style.transform);
  const pct = m ? Number(m[1]) : 0;
  return Math.round((pct / 100) * 10);
}

function shownValue(el: HTMLElement): string {
  return digitCells(el)
    .map(shownDigit)
    .join("");
}

describe("createOdometer — digit decomposition", () => {
  it("renders one digit cell per digit", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.setValue(12345);
    expect(digitCells(odo.el).length).toBe(5);
    expect(shownValue(odo.el)).toBe("12345");
  });

  it("each cell stacks all ten glyphs 0-9", () => {
    const odo = createOdometer();
    odo.setValue(7);
    const strip = odo.el.querySelector<HTMLElement>(".odo-strip")!;
    const glyphs = strip.querySelectorAll(".odo-glyph");
    expect(glyphs.length).toBe(10);
    expect(Array.from(glyphs, (g) => g.textContent).join("")).toBe("0123456789");
  });

  it("exposes the formatted value via data-value / aria-label", () => {
    const odo = createOdometer();
    odo.setValue(1234567);
    expect(odo.el.dataset.value).toBe("1,234,567");
    expect(odo.el.getAttribute("aria-label")).toBe("1,234,567");
  });
});

describe("createOdometer — separator placement", () => {
  it("inserts a comma every three digits by default", () => {
    const odo = createOdometer();
    odo.setValue(1234567);
    expect(seps(odo.el).length).toBe(2);
    expect(odo.el.dataset.value).toBe("1,234,567");
  });

  it("omits separators when groupSeparator is false", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.setValue(1234567);
    expect(seps(odo.el).length).toBe(0);
    expect(digitCells(odo.el).length).toBe(7);
  });

  it("uses no separator below 1000", () => {
    const odo = createOdometer();
    odo.setValue(999);
    expect(seps(odo.el).length).toBe(0);
    expect(digitCells(odo.el).length).toBe(3);
  });

  it("adds a separator exactly at 1000", () => {
    const odo = createOdometer();
    odo.setValue(1000);
    expect(seps(odo.el).length).toBe(1);
    expect(odo.el.dataset.value).toBe("1,000");
  });
});

describe("createOdometer — value growth / shrink rebuild", () => {
  it("grows the cell structure when the number gains digits", () => {
    const odo = createOdometer();
    odo.setValue(9998);
    expect(digitCells(odo.el).length).toBe(4);
    odo.setValue(10002);
    expect(digitCells(odo.el).length).toBe(5);
    expect(shownValue(odo.el)).toBe("10002");
    expect(seps(odo.el).length).toBe(1);
  });

  it("shrinks the cell structure when the number loses digits", () => {
    const odo = createOdometer();
    odo.setValue(12345);
    expect(digitCells(odo.el).length).toBe(5);
    odo.setValue(42);
    expect(digitCells(odo.el).length).toBe(2);
    expect(shownValue(odo.el)).toBe("42");
  });

  it("rolls in place when the digit count is unchanged", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.setValue(100);
    const before = odo.el.querySelector(".odo-strip");
    odo.setValue(999);
    const after = odo.el.querySelector(".odo-strip");
    // Same structure reused (no rebuild) — first strip node is identical.
    expect(after).toBe(before);
    expect(shownValue(odo.el)).toBe("999");
  });
});

describe("createOdometer — defensive inputs", () => {
  it("renders 0 as a single zero cell", () => {
    const odo = createOdometer();
    odo.setValue(0);
    expect(digitCells(odo.el).length).toBe(1);
    expect(shownValue(odo.el)).toBe("0");
    expect(odo.value()).toBe(0);
  });

  it("clamps negative values to 0", () => {
    const odo = createOdometer();
    odo.setValue(-500);
    expect(odo.el.dataset.value).toBe("0");
    expect(odo.value()).toBe(0);
  });

  it("treats non-finite values as 0", () => {
    const odo = createOdometer();
    odo.setValue(Number.NaN);
    expect(odo.el.dataset.value).toBe("0");
    odo.setValue(Number.POSITIVE_INFINITY);
    expect(odo.el.dataset.value).toBe("0");
  });

  it("rounds fractional values to the nearest integer", () => {
    const odo = createOdometer();
    odo.setValue(1234.6);
    expect(odo.el.dataset.value).toBe("1,235");
  });
});

describe("createOdometer — rapid retarget", () => {
  it("settles on the latest value when setValue is called repeatedly", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.setValue(100);
    odo.setValue(200);
    odo.setValue(305);
    expect(shownValue(odo.el)).toBe("305");
    expect(odo.value()).toBe(305);
  });
});
