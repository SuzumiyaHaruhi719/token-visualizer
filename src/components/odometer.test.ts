import { describe, it, expect } from "vitest";
import { createOdometer } from "./odometer";

// The global rAF stub in src/test-setup.ts advances a fake clock by a huge step
// each frame, so the eased continuous driver closes its gap and CONVERGES within
// a couple frames, then stops scheduling rAF (otherwise the synchronous stub
// would loop forever). We therefore assert on the SETTLED DOM (digit count,
// separators, decoded transforms), not on intermediate motion.

// Each strip holds 11 glyphs (0..9 then a wrap-around 0), so the wheel offset
// maps to translateY as (offset / 11) * 100%.
const STRIP_GLYPHS = 11;

function digitCells(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(".odo-digit"));
}

function seps(el: HTMLElement): HTMLElement[] {
  return Array.from(el.querySelectorAll<HTMLElement>(".odo-sep"));
}

/** The settled wheel offset of a digit cell, decoded from its strip transform. */
function wheelOffset(cell: HTMLElement): number {
  const strip = cell.querySelector<HTMLElement>(".odo-strip")!;
  const m = /translateY\(-([\d.]+)%\)/.exec(strip.style.transform);
  const pct = m ? Number(m[1]) : 0;
  return (pct / 100) * STRIP_GLYPHS;
}

// Decode the settled integer from the stacked wheel transforms. A real
// mechanical odometer parks higher wheels at FRACTIONAL offsets while a lower
// wheel sits mid-digit (e.g. value 305 -> tens wheel at 0.5 because 305/10 =
// 30.5). The correct digit for each place is therefore floor(offset) (with a
// tiny epsilon to absorb float error), MSB-first, which reconstructs the value.
function shownValue(el: HTMLElement): string {
  return digitCells(el)
    .map((cell) => Math.floor(wheelOffset(cell) + 1e-6) % 10)
    .join("");
}

describe("createOdometer — digit decomposition", () => {
  it("renders one digit cell per digit", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(12345);
    expect(digitCells(odo.el).length).toBe(5);
    expect(shownValue(odo.el)).toBe("12345");
  });

  it("each cell stacks 0-9 plus a trailing wrap-around 0", () => {
    const odo = createOdometer();
    odo.snapTo(7);
    const strip = odo.el.querySelector<HTMLElement>(".odo-strip")!;
    const glyphs = strip.querySelectorAll(".odo-glyph");
    expect(glyphs.length).toBe(11);
    // 0..9 then a duplicate 0 so 9 -> 0 wraps seamlessly.
    expect(Array.from(glyphs, (g) => g.textContent).join("")).toBe("01234567890");
  });

  it("exposes the formatted value via data-value / aria-label", () => {
    const odo = createOdometer();
    odo.snapTo(1234567);
    expect(odo.el.dataset.value).toBe("1,234,567");
    expect(odo.el.getAttribute("aria-label")).toBe("1,234,567");
  });
});

describe("createOdometer — separator placement", () => {
  it("inserts a comma every three digits by default", () => {
    const odo = createOdometer();
    odo.snapTo(1234567);
    expect(seps(odo.el).length).toBe(2);
    expect(odo.el.dataset.value).toBe("1,234,567");
  });

  it("omits separators when groupSeparator is false", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(1234567);
    expect(seps(odo.el).length).toBe(0);
    expect(digitCells(odo.el).length).toBe(7);
  });

  it("uses no separator below 1000", () => {
    const odo = createOdometer();
    odo.snapTo(999);
    expect(seps(odo.el).length).toBe(0);
    expect(digitCells(odo.el).length).toBe(3);
  });

  it("adds a separator exactly at 1000", () => {
    const odo = createOdometer();
    odo.snapTo(1000);
    expect(seps(odo.el).length).toBe(1);
    expect(odo.el.dataset.value).toBe("1,000");
  });
});

describe("createOdometer — value growth / shrink rebuild", () => {
  it("grows the cell structure when the number gains digits", () => {
    const odo = createOdometer();
    odo.snapTo(9998);
    expect(digitCells(odo.el).length).toBe(4);
    odo.setTarget(10002);
    expect(digitCells(odo.el).length).toBe(5);
    expect(shownValue(odo.el)).toBe("10002");
    expect(seps(odo.el).length).toBe(1);
  });

  it("shrinks the cell structure when the number loses digits", () => {
    const odo = createOdometer();
    odo.snapTo(12345);
    expect(digitCells(odo.el).length).toBe(5);
    // Downward target snaps (range switch), shrinking the structure.
    odo.setTarget(42);
    expect(digitCells(odo.el).length).toBe(2);
    expect(shownValue(odo.el)).toBe("42");
  });

  it("reuses the units strip node when rolling within the same digit count", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(100);
    const before = odo.el.querySelector(".odo-strip");
    odo.setTarget(999);
    const after = odo.el.querySelector(".odo-strip");
    expect(after).toBe(before);
    expect(shownValue(odo.el)).toBe("999");
  });
});

describe("createOdometer — defensive inputs", () => {
  it("renders 0 as a single zero cell", () => {
    const odo = createOdometer();
    odo.snapTo(0);
    expect(digitCells(odo.el).length).toBe(1);
    expect(shownValue(odo.el)).toBe("0");
    expect(odo.value()).toBe(0);
  });

  it("clamps negative values to 0", () => {
    const odo = createOdometer();
    odo.snapTo(-500);
    expect(odo.el.dataset.value).toBe("0");
    expect(odo.value()).toBe(0);
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
  it("converges to an upward target and settles on the exact value", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(100);
    odo.setTarget(305);
    // The eased driver converges within the fake clock's few frames and stops.
    expect(shownValue(odo.el)).toBe("305");
    expect(odo.el.dataset.value).toBe("305");
    expect(odo.value()).toBe(305);
  });

  it("snaps immediately on a downward target (range switch)", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(900);
    odo.setTarget(200);
    expect(shownValue(odo.el)).toBe("200");
    expect(odo.value()).toBe(200);
  });

  it("settles on the latest value when setTarget is called repeatedly", () => {
    const odo = createOdometer({ groupSeparator: false });
    odo.snapTo(100);
    odo.setTarget(200);
    odo.setTarget(305);
    expect(shownValue(odo.el)).toBe("305");
    expect(odo.value()).toBe(305);
  });

  it("setValue is an alias for snapTo (immediate set)", () => {
    const odo = createOdometer();
    odo.setValue(54321);
    expect(odo.el.dataset.value).toBe("54,321");
    expect(odo.value()).toBe(54321);
  });
});
