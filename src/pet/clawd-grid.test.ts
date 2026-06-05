import { describe, it, expect } from "vitest";
import { GRID, COLS, ROWS, partsOf, buildCells, ORANGE, EYE } from "./clawd-grid";

describe("clawd grid integrity", () => {
  it("is 12 cols x 8 rows", () => {
    expect(GRID.length).toBe(ROWS);
    expect(ROWS).toBe(8);
    for (const row of GRID) expect(row.length).toBe(COLS);
    expect(COLS).toBe(12);
  });

  it("has exactly 2 eyes", () => {
    const { eyes } = partsOf();
    expect(eyes.length).toBe(2);
    for (const e of eyes) expect(e.color).toBe(EYE);
  });

  it("has exactly 4 legs, each with cells", () => {
    const { legs } = partsOf();
    expect(legs.length).toBe(4);
    for (const leg of legs) {
      expect(leg.length).toBeGreaterThan(0);
      for (const c of leg) expect(c.color).toBe(ORANGE);
    }
  });

  it("splits arms into a left and a right group", () => {
    const { arml, armr } = partsOf();
    expect(arml.length).toBeGreaterThan(0);
    expect(armr.length).toBeGreaterThan(0);
    // left arm sits at low columns, right arm at high columns
    expect(Math.max(...arml.map((c) => c.col))).toBeLessThan(
      Math.min(...armr.map((c) => c.col)),
    );
  });

  it("has a non-empty body", () => {
    const { body } = partsOf();
    expect(body.length).toBeGreaterThan(0);
  });

  it("classifies every filled cell into exactly one part", () => {
    const cells = buildCells();
    const { body, eyes, arml, armr, legs } = partsOf();
    const legCount = legs.reduce((n, l) => n + l.length, 0);
    expect(body.length + eyes.length + arml.length + armr.length + legCount).toBe(
      cells.length,
    );
    // sanity: filled-cell count matches non-dot chars in the grid
    const filled = GRID.join("").replace(/\./g, "").length;
    expect(cells.length).toBe(filled);
  });
});
