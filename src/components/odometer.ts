// Continuous mechanical ODOMETER: renders a number as a row of fixed-width
// digit cells, each holding a vertical strip of the glyphs 0-9 plus a trailing
// duplicate 0 (so the wrap from 9 back to 0 is seamless). Unlike a discrete
// per-step roll, this odometer runs a SINGLE rAF driver that eases a `displayed`
// float toward a `target`, and positions every wheel by the CONTINUOUS value:
// the wheel at place p is offset by `frac = (value / 10^p) mod 10` (a float),
// so the units wheel rolls fastest, tens 10x slower, etc. — the classic
// mechanical-counter look where the whole stack is perpetually in motion.
//
// The displayed value is also written to `data-value` (and aria-label) so the
// number is readable to assertions / assistive tech without scraping the
// stacked glyph strips.
//
// Snap vs. roll: live increments within the same range call setTarget(), which
// slowly rolls UP toward the freshest total (latency does not matter — the
// readout is meant to look perpetually climbing). Range switches / first paint
// call snapTo(), which jumps immediately so we never roll for seconds across
// billions. A DOWNWARD target also snaps (totals only grow within a range; a
// smaller target means a range switch).
//
// NOTE: like src/lib/tween.ts we deliberately do NOT honor
// `prefers-reduced-motion`. The roll is the whole point.

import { formatInt } from "../lib/format";

// Glyphs 0-9 plus a trailing duplicate 0 => 11 cells in each strip. The
// fractional wheel position runs 0..10 (10 == the trailing 0, visually identical
// to 0) so a value crossing an integer boundary slides smoothly through 9 -> 0.
const DIGIT_BASE = 10; // base-10 places
const STRIP_GLYPHS = DIGIT_BASE + 1; // 0..9 then a wrap-around 0

// Easing tuned so closing a gap takes ~1.5-3s: each frame we move the remaining
// distance by EASE_FRACTION (geometric approach) but never less than
// MIN_VELOCITY units/sec, so even a tiny gap keeps visibly rolling rather than
// crawling asymptotically. CONVERGE_EPSILON ends the roll (and stops scheduling
// rAF) once we're within <1 of target — critical so the synchronous rAF test
// stub terminates instead of looping forever.
const EASE_FRACTION = 0.06; // fraction of remaining gap closed per ~16ms frame
const MIN_VELOCITY = 1.2; // minimum units per second so motion stays visible
const CONVERGE_EPSILON = 1; // snap + stop when within this of target
const NOMINAL_FRAME_MS = 16.6667; // reference frame for EASE_FRACTION scaling

export interface OdometerOptions {
  /** Insert static comma cells every 3 digits (default true). */
  groupSeparator?: boolean;
}

export interface OdometerHandle {
  /** The odometer root element to mount in the DOM. */
  readonly el: HTMLElement;
  /**
   * Set the value to slowly roll TOWARD. Upward gaps ease in over ~1.5-3s;
   * a downward target snaps immediately (range switch). Clamped >= 0;
   * non-finite -> 0.
   */
  setTarget(n: number): void;
  /** Jump immediately to `n` with no roll (range switch / first paint). */
  snapTo(n: number): void;
  /** Alias of snapTo, kept for callers that want a non-rolling set. */
  setValue(n: number): void;
  /** Current target value (the last value passed to setTarget / snapTo). */
  value(): number;
}

/** Normalize an incoming number to a non-negative, finite integer. */
function sanitize(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const rounded = Math.round(n);
  return rounded < 0 ? 0 : rounded;
}

/** Digit COUNT for a non-negative integer value (>= 1). */
function digitCount(n: number): number {
  return String(Math.max(0, Math.round(n))).length;
}

/**
 * Decompose a digit count into the cell layout, most-significant first. Each
 * entry is either a digit slot (units-first place index) or a comma separator.
 */
type Cell = { kind: "digit"; place: number } | { kind: "sep" };

function layoutFor(count: number, groupSeparator: boolean): Cell[] {
  const cells: Cell[] = [];
  for (let i = 0; i < count; i++) {
    // i is MSB-first; convert to place-from-right (0 = units).
    const place = count - 1 - i;
    if (groupSeparator && i > 0 && (place + 1) % 3 === 0) {
      cells.push({ kind: "sep" });
    }
    cells.push({ kind: "digit", place });
  }
  return cells;
}

/**
 * Create an odometer. Mount `handle.el`, then call `setTarget(n)` to roll or
 * `snapTo(n)` to jump. The first paint should use snapTo.
 */
export function createOdometer(opts: OdometerOptions = {}): OdometerHandle {
  const groupSeparator = opts.groupSeparator ?? true;

  const root = document.createElement("span");
  root.className = "odometer";
  root.setAttribute("role", "text");

  // Strips currently in the DOM, units-first (index 0 = units place). Preserved
  // across rebuilds so persisting places keep their wheel offset.
  let strips: HTMLElement[] = [];
  let displayed = 0; // continuously-eased float that drives the wheels
  let target = 0; // latest value to roll toward
  let frame = 0; // active rAF handle (0 == not scheduled)
  let lastTime: number | null = null; // previous frame timestamp for dt

  function buildStrip(): HTMLElement {
    const cell = document.createElement("span");
    cell.className = "odo-digit";
    const strip = document.createElement("span");
    strip.className = "odo-strip";
    for (let d = 0; d < STRIP_GLYPHS; d++) {
      const glyph = document.createElement("span");
      glyph.className = "odo-glyph";
      glyph.textContent = String(d % DIGIT_BASE);
      strip.appendChild(glyph);
    }
    cell.appendChild(strip);
    return cell;
  }

  /**
   * Position a strip so wheel offset `pos` (0..10, may be fractional) sits in
   * the viewport. The strip is STRIP_GLYPHS tall; one glyph == 1/STRIP_GLYPHS.
   */
  function place(strip: HTMLElement, pos: number): void {
    const pct = (pos / STRIP_GLYPHS) * 100;
    strip.style.transform = `translateY(-${pct}%)`;
  }

  /**
   * Continuous wheel offset for place `p` at the given (float) value: the
   * fractional position of digit p, so higher places turn 10x slower each.
   * Returns a value in [0, 10).
   */
  function wheelOffset(value: number, p: number): number {
    const scaled = value / Math.pow(DIGIT_BASE, p);
    const mod = scaled % DIGIT_BASE;
    return mod < 0 ? mod + DIGIT_BASE : mod;
  }

  /**
   * Rebuild the cell structure for `count` digits, MSB-first, with separators.
   * Existing strips (units-first) are reused where possible so persisting places
   * keep their wheel offset and roll instead of snapping.
   */
  function rebuild(count: number): void {
    const cells = layoutFor(count, groupSeparator);
    root.textContent = "";
    const prev = strips;
    const next: HTMLElement[] = [];
    for (const cell of cells) {
      if (cell.kind === "sep") {
        const sep = document.createElement("span");
        sep.className = "odo-sep";
        sep.textContent = ",";
        root.appendChild(sep);
        continue;
      }
      const existing = prev[cell.place];
      if (existing) {
        root.appendChild(existing.parentElement as HTMLElement);
        next[cell.place] = existing;
      } else {
        const digitCell = buildStrip();
        const strip = digitCell.querySelector<HTMLElement>(".odo-strip")!;
        root.appendChild(digitCell);
        next[cell.place] = strip;
      }
    }
    strips = next;
  }

  /** Paint all wheels for the current `displayed` float, rebuilding if needed. */
  function render(): void {
    // The number of digit columns tracks the larger of displayed/target so a
    // growing roll has the columns ready before it crosses the boundary.
    const count = Math.max(digitCount(displayed), digitCount(target), 1);
    if (strips.filter(Boolean).length !== count) {
      rebuild(count);
    }
    for (let p = 0; p < count; p++) {
      const strip = strips[p];
      if (!strip) continue;
      place(strip, wheelOffset(displayed, p));
    }
  }

  /** Update the readable value attributes from `displayed`. */
  function syncLabel(): void {
    root.dataset.value = formatInt(displayed);
    root.setAttribute("aria-label", root.dataset.value);
  }

  function stop(): void {
    if (frame && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(frame);
    }
    frame = 0;
    lastTime = null;
  }

  /** One driver frame: ease `displayed` toward `target`, render, reschedule. */
  function step(now: number): void {
    const dt = lastTime === null ? NOMINAL_FRAME_MS : Math.max(0, now - lastTime);
    lastTime = now;

    const gap = target - displayed;
    if (Math.abs(gap) < CONVERGE_EPSILON) {
      // Converged: snap exactly and STOP scheduling (terminates the test stub).
      displayed = target;
      render();
      syncLabel();
      stop();
      return;
    }

    // Geometric ease scaled to the real frame time, with a minimum velocity so
    // a near-converged gap still visibly rolls instead of crawling forever.
    const frames = dt / NOMINAL_FRAME_MS;
    const easeStep = gap * (1 - Math.pow(1 - EASE_FRACTION, frames));
    const minStep = Math.sign(gap) * MIN_VELOCITY * (dt / 1000);
    let move = Math.abs(easeStep) > Math.abs(minStep) ? easeStep : minStep;
    // Never overshoot the target.
    if (Math.abs(move) >= Math.abs(gap)) move = gap;

    displayed += move;
    render();
    syncLabel();

    if (typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(step);
    } else {
      // No rAF (test/edge): converge synchronously so we never stall mid-roll.
      displayed = target;
      render();
      syncLabel();
    }
  }

  function start(): void {
    if (frame) return; // already running
    if (typeof requestAnimationFrame !== "function") {
      // No rAF available: jump straight to target.
      displayed = target;
      render();
      syncLabel();
      return;
    }
    lastTime = null;
    frame = requestAnimationFrame(step);
  }

  function snapTo(n: number): void {
    const value = sanitize(n);
    target = value;
    displayed = value;
    stop();
    render();
    syncLabel();
  }

  function setTarget(n: number): void {
    const value = sanitize(n);
    // Downward target => range switch to a smaller total: snap, don't roll down
    // for ages. Equal target => nothing to do.
    if (value <= displayed) {
      snapTo(value);
      return;
    }
    target = value;
    start();
  }

  function value(): number {
    return target;
  }

  return { el: root, setTarget, snapTo, setValue: snapTo, value };
}
