// Clawd renderer: builds the grouped SVG part-rig from the grid,
// and maps a PetState to a wrapper animation class.

import { buildCells, CELL, COLS, ROWS, type Cell } from "./clawd-grid";
import type { PetState, PetStateKind } from "../lib/types";

const SVG_NS = "http://www.w3.org/2000/svg";

/** State -> wrapper class. The class drives per-part CSS animations. */
export function stateToClass(state: PetState): string {
  return `state-${state.kind}`;
}

/** Just the kind, for callers that only need the discriminant. */
export function stateKind(state: PetState): PetStateKind {
  return state.kind;
}

function groupForPart(part: Cell["part"], legIndex?: number): string {
  switch (part) {
    case "body":
      return "p-body";
    case "eyes":
      return "p-eyes";
    case "arml":
      return "p-arml";
    case "armr":
      return "p-armr";
    case "leg":
      return `p-leg p-leg${legIndex ?? 1}`;
  }
}

/**
 * Build the Clawd SVG. Cells are grouped by animated part so CSS can target
 * each <g class="p-..."> independently. Returns a detached <svg> element.
 */
export function buildClawdSvg(doc: Document = document): SVGSVGElement {
  const svg = doc.createElementNS(SVG_NS, "svg") as SVGSVGElement;
  svg.setAttribute("class", "clawd");
  svg.setAttribute("viewBox", `0 0 ${COLS * CELL} ${ROWS * CELL}`);
  svg.setAttribute("width", String(COLS * CELL));
  svg.setAttribute("height", String(ROWS * CELL));
  svg.setAttribute("shape-rendering", "crispEdges");

  // Group cells by their target <g> class so each part is one group.
  const groups = new Map<string, SVGGElement>();
  const order = ["p-body", "p-arml", "p-armr", "p-eyes"]; // legs appended after

  const ensureGroup = (cls: string): SVGGElement => {
    let g = groups.get(cls);
    if (!g) {
      g = doc.createElementNS(SVG_NS, "g") as SVGGElement;
      g.setAttribute("class", cls);
      groups.set(cls, g);
    }
    return g;
  };

  for (const cell of buildCells()) {
    const cls = groupForPart(cell.part, cell.legIndex);
    const g = ensureGroup(cls);
    const rect = doc.createElementNS(SVG_NS, "rect");
    rect.setAttribute("x", String(cell.col * CELL));
    rect.setAttribute("y", String(cell.row * CELL));
    rect.setAttribute("width", String(CELL));
    rect.setAttribute("height", String(CELL));
    rect.setAttribute("fill", cell.color);
    g.appendChild(rect);
  }

  // Append groups in a stable, layered order (body first, eyes on top, legs last).
  const legClasses = [...groups.keys()]
    .filter((k) => k.startsWith("p-leg"))
    .sort();
  for (const cls of order) {
    const g = groups.get(cls);
    if (g) svg.appendChild(g);
  }
  for (const cls of legClasses) {
    const g = groups.get(cls);
    if (g) svg.appendChild(g);
  }

  return svg;
}
