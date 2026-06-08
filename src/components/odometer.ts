// Rolling number with two render modes:
//   - plain  (default): one cohesive formatted number (used for per-model rows).
//   - reels  (opts.reels): a slot-machine vertical digit roll — each digit is a
//     vertical strip of 0-9 that rolls; used for the big total.
//
// Both modes share ONE driver: the displayed value eases toward the latest
// target on a slow exponential approach (time-constant TAU_MS), so a single
// update keeps rolling for a few seconds. Token totals only change when a message
// completes (every few seconds), so the slow roll means each change is still
// rolling when the next arrives — the number rolls continuously during active
// use instead of snapping and freezing. The lag does not matter (the user wants
// the motion). A downward target snaps (range switch to a smaller total). We
// deliberately do NOT honor `prefers-reduced-motion` (the roll is the point).
//
// Reels-mode notes (lessons from earlier iterations):
//   - The UNITS reel rolls continuously; higher reels sit on their exact integer
//     digit and only roll in the last (1-CARRY_START) before they carry — so a
//     settled number reads cleanly (crisp, aligned) instead of every reel parking
//     at a fractional offset (which looked fragmented).
//   - Glyphs are colored a SOLID color (no background-clip:text gradient, which
//     painted dark boxes in the clipped cells); the glow is a CONTAINER filter,
//     not a per-glyph text-shadow (which a clipped cell would box in).

import { formatInt } from "../lib/format";

// Driver tuning. TAU_MS is the SLOW continuous roll for live increments (keeps
// rolling for seconds). TAU_FAST is the quick transition used on a tab switch,
// so the number rolls smoothly (and bidirectionally) to the new range's total
// in ~1s instead of snapping — a seamless hand-off, no stop.
const TAU_MS = 2000;
const TAU_FAST_MS = 320;
const FAST_WINDOW_MS = 1300; // how long a tab-switch transition stays fast
const MIN_UNITS_PER_SEC = 8;
const CONVERGE_EPSILON = 1;
const NOMINAL_FRAME_MS = 16.6667;

// Reel geometry: glyphs 0-9 plus a trailing duplicate 0 (=> 11) so 9 -> 0 wraps
// seamlessly. A higher reel only rolls in the final (1 - CARRY_START) of its
// approach, i.e. as the reel below it wraps 9 -> 0.
const DIGIT_BASE = 10;
const STRIP_GLYPHS = DIGIT_BASE + 1;
const CARRY_START = 0.9;

export interface OdometerOptions {
  /** Insert a comma every three digits (default true). */
  groupSeparator?: boolean;
  /** Render as a slot-machine vertical digit roll instead of plain text. */
  reels?: boolean;
}

export interface OdometerHandle {
  readonly el: HTMLElement;
  /** Roll continuously toward `n` (a downward jump snaps — range switch). */
  setTarget(n: number): void;
  /** Jump immediately to `n` with no roll (first paint). */
  snapTo(n: number): void;
  /** Alias of snapTo, for callers that want a non-rolling set. */
  setValue(n: number): void;
  /** Quick bidirectional roll to `n` over ~1s (tab switch — seamless hand-off). */
  transitionTo(n: number): void;
  /** The latest value passed to setTarget / snapTo / transitionTo. */
  value(): number;
}

/** Normalize to a non-negative finite integer. */
function sanitize(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const r = Math.round(n);
  return r < 0 ? 0 : r;
}

/** Digit count of a non-negative integer (>= 1). */
function digitCount(n: number): number {
  return String(Math.max(0, Math.round(n))).length;
}

type Cell = { kind: "digit"; place: number } | { kind: "sep" };

/** Cell layout for `count` digits, MSB-first, with optional comma separators. */
function layoutFor(count: number, groupSeparator: boolean): Cell[] {
  const cells: Cell[] = [];
  for (let i = 0; i < count; i++) {
    const place = count - 1 - i; // place-from-right (0 = units)
    if (groupSeparator && i > 0 && (place + 1) % 3 === 0) cells.push({ kind: "sep" });
    cells.push({ kind: "digit", place });
  }
  return cells;
}

export function createOdometer(opts: OdometerOptions = {}): OdometerHandle {
  const reels = opts.reels ?? false;
  const groupSeparator = opts.groupSeparator ?? true;

  const root = document.createElement("span");
  root.className = reels ? "odometer odometer-reels" : "odometer";
  root.setAttribute("role", "text");

  let displayed = 0; // value currently painted (a float, eased)
  let latest = 0; // most recent target (returned by value())
  let frame = 0; // active rAF handle (0 == not scheduled)
  let lastTime: number | null = null; // previous frame timestamp for dt
  // While `now < fastUntil` the ease uses TAU_FAST (a tab-switch transition);
  // otherwise the slow live-roll TAU. Computed per-frame so a heartbeat update
  // mid-transition never downgrades the speed.
  let fastUntil = 0;
  let strips: HTMLElement[] = []; // reel strips, units-first (reels mode only)

  /** Monotonic clock shared with the rAF timestamp (performance.now based). */
  function clock(): number {
    return typeof performance !== "undefined" && typeof performance.now === "function"
      ? performance.now()
      : Date.now();
  }

  // --- reel rendering -----------------------------------------------------
  function revealEnteringCell(cell: HTMLElement): void {
    if (typeof requestAnimationFrame !== "function") {
      cell.classList.remove("odo-enter");
      return;
    }
    requestAnimationFrame(() => cell.classList.remove("odo-enter"));
  }

  function buildStrip(entering = false): HTMLElement {
    const cell = document.createElement("span");
    cell.className = entering ? "odo-digit odo-enter" : "odo-digit";
    const strip = document.createElement("span");
    strip.className = "odo-strip";
    for (let d = 0; d < STRIP_GLYPHS; d++) {
      const glyph = document.createElement("span");
      glyph.className = "odo-glyph";
      glyph.textContent = String(d % DIGIT_BASE);
      strip.appendChild(glyph);
    }
    cell.appendChild(strip);
    if (entering) revealEnteringCell(cell);
    return cell;
  }

  /** Position a strip so wheel offset `pos` (0..10) sits in the 1-glyph window. */
  function place(strip: HTMLElement, pos: number): void {
    strip.style.transform = `translateY(-${(pos / STRIP_GLYPHS) * 100}%)`;
  }

  /** Wheel offset for place `p` at float `value` (units continuous, higher carry). */
  function wheelOffset(value: number, p: number): number {
    if (p === 0) {
      const u = value % DIGIT_BASE;
      return u < 0 ? u + DIGIT_BASE : u;
    }
    const placeVal = value / Math.pow(DIGIT_BASE, p);
    const digit = ((Math.floor(placeVal) % DIGIT_BASE) + DIGIT_BASE) % DIGIT_BASE;
    const frac = placeVal - Math.floor(placeVal);
    const roll = frac > CARRY_START ? (frac - CARRY_START) / (1 - CARRY_START) : 0;
    return digit + roll;
  }

  /** Rebuild reel cells for `count` digits, reusing persisting strips. */
  function rebuildReels(count: number): void {
    const cells = layoutFor(count, groupSeparator);
    const animateStructuralEntry = root.childElementCount > 0;
    root.textContent = "";
    const prev = strips;
    const next: HTMLElement[] = [];
    for (const cell of cells) {
      if (cell.kind === "sep") {
        const sep = document.createElement("span");
        sep.className = animateStructuralEntry ? "odo-sep odo-enter" : "odo-sep";
        sep.textContent = ",";
        root.appendChild(sep);
        if (animateStructuralEntry) revealEnteringCell(sep);
        continue;
      }
      const existing = prev[cell.place];
      if (existing) {
        root.appendChild(existing.parentElement as HTMLElement);
        next[cell.place] = existing;
      } else {
        const digitCell = buildStrip(animateStructuralEntry);
        root.appendChild(digitCell);
        next[cell.place] = digitCell.querySelector<HTMLElement>(".odo-strip")!;
      }
    }
    strips = next;
  }

  function syncLabel(): void {
    const text = formatInt(displayed);
    root.dataset.value = text;
    root.setAttribute("aria-label", text);
  }

  function render(): void {
    if (reels) {
      const count = Math.max(digitCount(displayed), digitCount(latest), 1);
      if (strips.filter(Boolean).length !== count) rebuildReels(count);
      for (let p = 0; p < count; p++) {
        const strip = strips[p];
        if (strip) place(strip, wheelOffset(displayed, p));
      }
      syncLabel();
    } else {
      const text = formatInt(displayed);
      root.textContent = text;
      root.dataset.value = text;
      root.setAttribute("aria-label", text);
    }
  }

  // --- driver (continuous ease toward latest) -----------------------------
  function stop(): void {
    if (frame && typeof cancelAnimationFrame === "function") cancelAnimationFrame(frame);
    frame = 0;
    lastTime = null;
  }

  function step(now: number): void {
    const dt = lastTime === null ? NOMINAL_FRAME_MS : Math.max(0, now - lastTime);
    lastTime = now;

    const gap = latest - displayed;
    if (Math.abs(gap) < CONVERGE_EPSILON) {
      displayed = latest;
      render();
      stop();
      return;
    }

    // Fast during a tab-switch transition window, slow for live rolls.
    const tau = now < fastUntil ? TAU_FAST_MS : TAU_MS;
    // Exponential approach (works both directions for tab-switch transitions),
    // with a velocity floor so the tail still visibly rolls.
    let move = gap * (1 - Math.exp(-dt / tau));
    const minMove = Math.sign(gap) * MIN_UNITS_PER_SEC * (dt / 1000);
    if (Math.abs(move) < Math.abs(minMove)) move = minMove;
    if (Math.abs(move) > Math.abs(gap)) move = gap;

    displayed += move;
    render();

    if (typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(step);
    } else {
      displayed = latest; // no rAF (tests): settle immediately
      render();
      frame = 0;
    }
  }

  function start(): void {
    if (frame) return;
    if (typeof requestAnimationFrame !== "function") {
      displayed = latest;
      render();
      return;
    }
    lastTime = null;
    frame = requestAnimationFrame(step);
  }

  function snapTo(n: number): void {
    const v = sanitize(n);
    latest = v;
    displayed = v;
    stop();
    render();
  }

  function setTarget(n: number): void {
    const v = sanitize(n);
    // Raise-only: live/creep totals only grow. A target at or below the current
    // value is ignored (keeps any in-flight roll going) rather than snapping
    // backward — so the perpetual creep that drives the always-on roll is never
    // yanked back when a slightly-lower real total arrives, and a live update
    // never disturbs a downward tab-switch transition. (Range switches go
    // through transitionTo, which handles downward moves.)
    if (v <= displayed) return;
    latest = v;
    start();
  }

  /**
   * Roll quickly (and bidirectionally) toward `n` over ~1s — used on a tab
   * switch so the number smoothly hands off to the new range's total instead of
   * freezing/snapping. Opens a fast window so the ease is quick even if the live
   * heartbeat also updates the target mid-spin. Rolls UP or DOWN as needed.
   */
  function transitionTo(n: number): void {
    const v = sanitize(n);
    fastUntil = clock() + FAST_WINDOW_MS;
    if (v === displayed) {
      render();
      return;
    }
    latest = v;
    start();
  }

  function value(): number {
    return latest;
  }

  render();
  return { el: root, setTarget, snapTo, setValue: snapTo, transitionTo, value };
}
