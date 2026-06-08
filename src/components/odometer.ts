// Rolling-digit ODOMETER: renders an integer as a row of fixed-width digit
// cells, each holding a vertical strip of the glyphs 0-9. Showing digit `d`
// means translating the strip so glyph `d` sits in the cell's viewport (the
// cell clips with overflow:hidden). setValue() rolls each column from its
// current glyph to the target via requestAnimationFrame + easeOutCubic, with a
// per-place STAGGER so the units digit leads and higher places lag slightly —
// the mechanical-counter feel. Columns whose glyph is unchanged do not roll.
//
// The displayed value is also written to `data-value` (and aria-label) so the
// number is readable to assertions / assistive tech without scraping the ten
// stacked glyphs of every strip.
//
// NOTE: like src/lib/tween.ts we deliberately do NOT honor
// `prefers-reduced-motion`. The roll is the whole point; the only time we snap
// is when requestAnimationFrame is unavailable (test env safety) or the column
// glyph is unchanged.

import { formatInt } from "../lib/format";

const GLYPH_COUNT = 10; // digits 0-9 stacked per strip
const ROLL_DURATION_MS = 560; // base roll time for the units column
const PER_PLACE_LAG_MS = 55; // each higher place starts/settles this much later
const MAX_EXTRA_DURATION_MS = 220; // cap the added lag so big numbers stay snappy

export interface OdometerOptions {
  /** Insert static comma cells every 3 digits (default true). */
  groupSeparator?: boolean;
}

export interface OdometerHandle {
  /** The odometer root element to mount in the DOM. */
  readonly el: HTMLElement;
  /** Roll the odometer to `n` (clamped >= 0; non-finite -> 0). */
  setValue(n: number): void;
  /** Current target value (the last value passed to setValue). */
  value(): number;
}

/** Cubic ease-out: fast start, gentle settle (matches tween.ts). */
function easeOutCubic(t: number): number {
  const clamped = t < 0 ? 0 : t > 1 ? 1 : t;
  return 1 - Math.pow(1 - clamped, 3);
}

/** Normalize an incoming number to a non-negative, finite integer. */
function sanitize(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const rounded = Math.round(n);
  return rounded < 0 ? 0 : rounded;
}

/** Digits of `n` as an array of place values, most-significant first. */
function digitsOf(n: number): number[] {
  return String(n).split("").map((c) => Number(c));
}

/**
 * Decompose a digit count into the cell layout, most-significant first. Each
 * entry is either a digit slot (index into the value's digit array) or a comma
 * separator. Example for 5 digits with separators: [d, d, sep, d, d, d].
 */
type Cell = { kind: "digit"; place: number } | { kind: "sep" };

function layoutFor(digitCount: number, groupSeparator: boolean): Cell[] {
  const cells: Cell[] = [];
  for (let i = 0; i < digitCount; i++) {
    // Place from the RIGHT (0 = units) drives separator placement.
    const fromRight = digitCount - 1 - i;
    if (groupSeparator && i > 0 && fromRight % 3 === 2) {
      cells.push({ kind: "sep" });
    }
    cells.push({ kind: "digit", place: i });
  }
  return cells;
}

// Per-column animation state lives on the strip element via this map so rapid
// setValue() calls can cancel the in-flight frame and retarget smoothly.
interface ColumnAnim {
  frame: number;
  from: number; // fractional glyph position the roll started from
  to: number; // target glyph
  startTime: number | null;
  duration: number;
  delay: number;
}

/**
 * Create an odometer. Mount `handle.el`, then call `handle.setValue(n)` to roll.
 * The first setValue paints instantly (no prior glyph to roll from).
 */
export function createOdometer(opts: OdometerOptions = {}): OdometerHandle {
  const groupSeparator = opts.groupSeparator ?? true;

  const root = document.createElement("span");
  root.className = "odometer";
  root.setAttribute("role", "text");

  // Strips currently in the DOM, units-first (index 0 = units column). Lets us
  // preserve columns across setValue calls so unchanged places stay put and
  // changed ones roll from their existing glyph.
  let strips: HTMLElement[] = [];
  const anims = new Map<HTMLElement, ColumnAnim>();
  let current = 0;

  function buildStrip(): HTMLElement {
    const cell = document.createElement("span");
    cell.className = "odo-digit";
    const strip = document.createElement("span");
    strip.className = "odo-strip";
    for (let d = 0; d < GLYPH_COUNT; d++) {
      const glyph = document.createElement("span");
      glyph.className = "odo-glyph";
      glyph.textContent = String(d);
      strip.appendChild(glyph);
    }
    cell.appendChild(strip);
    return cell;
  }

  /** Position a strip so glyph `pos` (may be fractional) is in the viewport. */
  function place(strip: HTMLElement, pos: number): void {
    // Each glyph is 1em tall; translate up by pos glyphs (percentage of strip).
    const pct = (pos / GLYPH_COUNT) * 100;
    strip.style.transform = `translateY(-${pct}%)`;
  }

  /**
   * Rebuild the cell structure for a given digit count, most-significant first,
   * with separators. Existing strips (units-first) are reused where possible so
   * persisting columns keep their glyph and roll instead of snapping.
   */
  function rebuild(digitCount: number): void {
    cancelAll();
    const cells = layoutFor(digitCount, groupSeparator);
    root.textContent = "";

    // Reuse existing strips (units-first) so persisting columns keep their
    // current transform and roll; new leading places get fresh strips parked
    // at glyph 0 so they roll in from 0.
    const prev = strips;

    // Render cells MSB-first into the DOM, wiring up the per-place strip refs.
    const nextStrips: HTMLElement[] = [];
    for (const cell of cells) {
      if (cell.kind === "sep") {
        const sep = document.createElement("span");
        sep.className = "odo-sep";
        sep.textContent = ",";
        root.appendChild(sep);
        continue;
      }
      // cell.place is MSB-first index; convert to units-first index.
      const unitsIndex = digitCount - 1 - cell.place;
      const existing = prev[unitsIndex];
      if (existing) {
        // Reuse the whole digit cell (keeps its current transform).
        root.appendChild(existing.parentElement as HTMLElement);
        nextStrips[unitsIndex] = existing;
      } else {
        const digitCell = buildStrip();
        const strip = digitCell.querySelector<HTMLElement>(".odo-strip")!;
        place(strip, 0);
        root.appendChild(digitCell);
        nextStrips[unitsIndex] = strip;
      }
    }

    strips = nextStrips;
  }

  function cancelColumn(strip: HTMLElement): void {
    const a = anims.get(strip);
    if (a && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(a.frame);
    }
    anims.delete(strip);
  }

  function cancelAll(): void {
    for (const strip of strips) {
      if (strip) cancelColumn(strip);
    }
  }

  /** Roll one column's strip from its current position to glyph `to`. */
  function rollColumn(strip: HTMLElement, to: number, placeIndex: number): void {
    cancelColumn(strip);

    // Current fractional glyph position (from a transform mid-roll or settled).
    const from = currentPos(strip);
    if (Math.round(from) === to && Number.isInteger(from)) {
      // Unchanged glyph — leave it parked, don't roll.
      place(strip, to);
      return;
    }

    if (typeof requestAnimationFrame !== "function") {
      place(strip, to);
      return;
    }

    // Stagger: higher place-values lag and roll a touch longer.
    const extra = Math.min(placeIndex * PER_PLACE_LAG_MS, MAX_EXTRA_DURATION_MS);
    const anim: ColumnAnim = {
      frame: 0,
      from,
      to,
      startTime: null,
      duration: ROLL_DURATION_MS + extra,
      delay: extra,
    };

    const step = (now: number): void => {
      if (anim.startTime === null) anim.startTime = now;
      const elapsed = now - anim.startTime - anim.delay;
      if (elapsed < 0) {
        anim.frame = requestAnimationFrame(step);
        return;
      }
      const progress = anim.duration > 0 ? elapsed / anim.duration : 1;
      const eased = easeOutCubic(progress);
      const pos = anim.from + (anim.to - anim.from) * eased;
      place(strip, pos);
      if (progress < 1) {
        anim.frame = requestAnimationFrame(step);
      } else {
        place(strip, anim.to);
        anims.delete(strip);
      }
    };

    anim.frame = requestAnimationFrame(step);
    anims.set(strip, anim);
  }

  /**
   * Read the strip's current fractional glyph position from its inline
   * transform. During a retarget this is the live mid-roll position, so a new
   * roll continues smoothly from wherever the column currently sits.
   */
  function currentPos(strip: HTMLElement): number {
    const t = strip.style.transform;
    const m = /translateY\(-([\d.]+)%\)/.exec(t);
    if (!m) return 0;
    const pct = Number(m[1]);
    return (pct / 100) * GLYPH_COUNT;
  }

  function setValue(n: number): void {
    const value = sanitize(n);
    current = value;
    root.dataset.value = formatInt(value);
    root.setAttribute("aria-label", root.dataset.value);

    const digits = digitsOf(value); // MSB-first
    const digitCount = digits.length;

    // Rebuild structure if the digit count changed (grow/shrink).
    const haveColumns = strips.filter(Boolean).length;
    if (haveColumns !== digitCount) {
      rebuild(digitCount);
    }

    // Roll each column to its target glyph, units-first (place index = i).
    for (let i = 0; i < digitCount; i++) {
      const strip = strips[i];
      if (!strip) continue;
      const msbIndex = digitCount - 1 - i; // units-first -> MSB-first
      rollColumn(strip, digits[msbIndex], i);
    }
  }

  function value(): number {
    return current;
  }

  return { el: root, setValue, value };
}
