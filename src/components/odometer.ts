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

// Interpolation-buffer playback: we deliberately render ONE telemetry sample
// behind real time and always interpolate between the two most recent buffered
// samples. As long as new samples keep arriving (the dashboard feeds one every
// ~500ms), there is always a "next" sample to roll toward, so the odometer rolls
// CONTINUOUSLY instead of easing to the latest value and stopping. RENDER_DELAY
// is the lag (a bit more than the sample cadence so a next sample is always
// buffered); when samples stop, playback drains to the last value and halts.
const RENDER_DELAY_MS = 750;
// A higher wheel (tens and up) only rolls during the final (1 - CARRY_START) of
// its approach — i.e. when the wheel below it is wrapping 9->0 — so settled
// numbers read crisply instead of every wheel parking at a fractional offset.
const CARRY_START = 0.9;

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
  let displayed = 0; // the value currently painted on the wheels (a float)
  let latest = 0; // most recent value pushed (returned by value())
  // Buffer of recent samples in ASCENDING time order; the driver plays back
  // through these RENDER_DELAY_MS behind real time so it always has a segment
  // to interpolate (continuous roll). Timestamps use `clock()` (== rAF's clock).
  let samples: { v: number; t: number }[] = [];
  let frame = 0; // active rAF handle (0 == not scheduled)

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
   * Wheel offset for place `p` at the given (float) value, in [0, 10].
   *
   * The UNITS wheel (p=0) rolls fully continuously (`value mod 10`) so the
   * lowest digit is always spinning. Higher wheels sit CRISPLY on their exact
   * integer digit and only roll during the final [`CARRY_START`..1) of their
   * approach — i.e. exactly when the wheel below wraps 9->0 — so a settled
   * number reads cleanly (e.g. 449,297,072 shows 7 on the tens wheel, not 7.2)
   * instead of every wheel parking mid-glyph.
   */
  function wheelOffset(value: number, p: number): number {
    if (p === 0) {
      const u = value % DIGIT_BASE;
      return u < 0 ? u + DIGIT_BASE : u;
    }
    const place = value / Math.pow(DIGIT_BASE, p);
    const digit = ((Math.floor(place) % DIGIT_BASE) + DIGIT_BASE) % DIGIT_BASE;
    const frac = place - Math.floor(place);
    const roll = frac > CARRY_START ? (frac - CARRY_START) / (1 - CARRY_START) : 0;
    return digit + roll;
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
    // The number of digit columns tracks the larger of displayed/latest so a
    // growing roll has the columns ready before it crosses the boundary.
    const count = Math.max(digitCount(displayed), digitCount(latest), 1);
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

  /** Monotonic clock shared by sample timestamps and the rAF callback. */
  function clock(): number {
    return typeof performance !== "undefined" && typeof performance.now === "function"
      ? performance.now()
      : Date.now();
  }

  function stop(): void {
    if (frame && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(frame);
    }
    frame = 0;
  }

  /** Drop samples older than the one bracketing the current render time. */
  function trimSamples(renderTime: number): void {
    // Keep the last sample at/just-before renderTime plus everything after, so
    // the bracketing pair survives. Always retain at least the final 2.
    let keepFrom = 0;
    for (let i = 0; i < samples.length - 1; i++) {
      if (samples[i + 1].t <= renderTime) keepFrom = i + 1;
    }
    if (keepFrom > 0) samples = samples.slice(keepFrom);
  }

  /**
   * One playback frame: interpolate `displayed` between the two buffered samples
   * that bracket `now - RENDER_DELAY_MS`, render, and reschedule until playback
   * has drained to the latest sample (then stop so the rAF test stub terminates).
   */
  function step(now: number): void {
    const renderTime = now - RENDER_DELAY_MS;

    // Find bracket: `a` = last sample at/before renderTime, `b` = first after it.
    let a = samples[0];
    let b: { v: number; t: number } | null = null;
    for (const s of samples) {
      if (s.t <= renderTime) a = s;
      else {
        b = s;
        break;
      }
    }

    if (b) {
      const span = Math.max(1, b.t - a.t);
      const p = Math.min(1, Math.max(0, (renderTime - a.t) / span));
      displayed = a.v + (b.v - a.v) * p;
    } else {
      // Drained: render time has reached the most recent sample.
      displayed = a.v;
    }
    render();
    syncLabel();
    trimSamples(renderTime);

    const last = samples[samples.length - 1];
    if (b !== null && renderTime < last.t && typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(step);
    } else {
      // Idle: settle exactly on the latest value and stop scheduling.
      displayed = last.v;
      render();
      syncLabel();
      samples = [last];
      frame = 0;
    }
  }

  function start(): void {
    if (frame) return; // already running
    if (typeof requestAnimationFrame !== "function") {
      // No rAF (test/edge): jump straight to the latest value.
      displayed = latest;
      render();
      syncLabel();
      return;
    }
    frame = requestAnimationFrame(step);
  }

  function snapTo(n: number): void {
    const v = sanitize(n);
    latest = v;
    displayed = v;
    samples = [];
    stop();
    render();
    syncLabel();
  }

  /**
   * Push a new telemetry sample to roll toward. The driver plays back through
   * the buffer RENDER_DELAY_MS behind real time, so consecutive samples produce
   * one continuous roll. A downward jump (range switch to a smaller total) snaps
   * instead of rolling backward for ages.
   */
  function setTarget(n: number): void {
    const v = sanitize(n);
    if (v < displayed) {
      snapTo(v);
      return;
    }
    latest = v;
    if (v === displayed && samples.length === 0) {
      // Nothing to animate yet and no roll in flight — just paint it.
      render();
      syncLabel();
      return;
    }
    const t = clock();
    if (samples.length === 0) {
      // Seed a starting point one delay back so we roll from where we are now.
      samples.push({ v: displayed, t: t - RENDER_DELAY_MS });
    }
    samples.push({ v, t });
    start();
  }

  function value(): number {
    return latest;
  }

  return { el: root, setTarget, snapTo, setValue: snapTo, value };
}
