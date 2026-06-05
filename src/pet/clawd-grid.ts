// Clawd: 12 cols x 8 rows, extracted 1:1 from the official reference sprite.
// Single source of truth for the SVG part-rig.

export const GRID = [
  "..OOOOOOOO..",
  "..OKOOOOKO..",
  "OOOOOOOOOOOO",
  "OOOOOOOOOOOO",
  "..OOOOOOOO..",
  "..OOOOOOOO..",
  "..O.O..O.O..",
  "..O.O..O.O..",
];

export const ORANGE = "#da7757";
export const EYE = "#171717";
export const CELL = 12; // px per grid cell
export const COLS = 12;
export const ROWS = 8;

export type PartName = "body" | "eyes" | "arml" | "armr" | "leg";

export interface Cell {
  /** grid column (x) */
  col: number;
  /** grid row (y) */
  row: number;
  /** which animated part this cell belongs to */
  part: PartName;
  /** fill color */
  color: string;
  /** for legs: 1..4 (left-to-right); otherwise undefined */
  legIndex?: number;
}

/**
 * Classify a filled cell into an animated part.
 * Part map (per the design contract):
 *   - 'K'                              -> eyes
 *   - rows 2-3, cols {0,1}             -> arml (left arm)
 *   - rows 2-3, cols {10,11}           -> armr (right arm)
 *   - rows 6-7, cols {2,4,7,9}         -> leg1..leg4
 *   - any other 'O'                    -> body
 */
function classify(col: number, row: number, ch: string): Omit<Cell, "col" | "row"> | null {
  if (ch === "K") return { part: "eyes", color: EYE };
  if (ch !== "O") return null; // '.' = empty

  if (row === 2 || row === 3) {
    if (col === 0 || col === 1) return { part: "arml", color: ORANGE };
    if (col === 10 || col === 11) return { part: "armr", color: ORANGE };
  }
  if (row === 6 || row === 7) {
    const legCols = [2, 4, 7, 9];
    const idx = legCols.indexOf(col);
    if (idx !== -1) return { part: "leg", color: ORANGE, legIndex: idx + 1 };
  }
  return { part: "body", color: ORANGE };
}

/** Flatten the GRID into a list of drawable, classified cells. */
export function buildCells(grid: readonly string[] = GRID): Cell[] {
  const cells: Cell[] = [];
  for (let row = 0; row < grid.length; row++) {
    const line = grid[row];
    for (let col = 0; col < line.length; col++) {
      const meta = classify(col, row, line[col]);
      if (meta) cells.push({ col, row, ...meta });
    }
  }
  return cells;
}

/** Summary of part composition — used by integrity tests and rendering. */
export interface GridParts {
  body: Cell[];
  eyes: Cell[];
  arml: Cell[];
  armr: Cell[];
  legs: Cell[][]; // legs[0] = leg1 cells, etc.
}

export function partsOf(grid: readonly string[] = GRID): GridParts {
  const cells = buildCells(grid);
  const legs: Cell[][] = [[], [], [], []];
  const parts: GridParts = { body: [], eyes: [], arml: [], armr: [], legs };
  for (const c of cells) {
    switch (c.part) {
      case "body":
        parts.body.push(c);
        break;
      case "eyes":
        parts.eyes.push(c);
        break;
      case "arml":
        parts.arml.push(c);
        break;
      case "armr":
        parts.armr.push(c);
        break;
      case "leg":
        legs[(c.legIndex ?? 1) - 1].push(c);
        break;
    }
  }
  return parts;
}
