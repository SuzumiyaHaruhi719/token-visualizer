// Continuous rolling number.
//
// Renders ONE cohesive number (with thousands separators) that smoothly,
// continuously rolls UP toward the latest telemetry value. Earlier this drew
// per-digit "wheels", but the independent fractional wheels looked fragmented
// (割裂) and their overflow-clipped cells painted ugly dark boxes; rendering the
// whole number as a single counting value reads as a clean roll and avoids both.
//
// Continuity uses an INTERPOLATION BUFFER: playback runs RENDER_DELAY_MS behind
// real time and interpolates between the two most recent samples, so as long as
// new samples keep arriving (the dashboard feeds one every ~500ms) there is
// always a "next" sample to roll toward — the number keeps rolling instead of
// snapping to the latest and freezing. When samples stop, it drains to the last
// value and halts (nothing to animate when no new tokens arrive). We deliberately
// do NOT honor `prefers-reduced-motion` (the roll is the whole point).

import { formatInt } from "../lib/format";

// Roll speed. The displayed value eases toward the latest target on an
// exponential approach with time-constant TAU_MS, so a single update keeps
// visibly rolling for several seconds (~95% closed in ~3*TAU). Token totals only
// change when a message completes (every few seconds), so a SLOW roll means each
// change is still rolling when the next arrives — the number rolls continuously
// during active use instead of snapping and freezing. Latency does not matter
// here (the user wants the motion); the displayed value simply lags the latest.
// MIN_UNITS_PER_SEC keeps it visibly creeping near the end; once within
// CONVERGE_EPSILON it snaps and stops scheduling (so the rAF test stub ends).
const TAU_MS = 2000;
const MIN_UNITS_PER_SEC = 8;
const CONVERGE_EPSILON = 1;
const NOMINAL_FRAME_MS = 16.6667;

export interface OdometerOptions {
  /** Accepted for API compatibility; thousands separators are always on. */
  groupSeparator?: boolean;
}

export interface OdometerHandle {
  /** The element to mount in the DOM. */
  readonly el: HTMLElement;
  /** Roll continuously toward `n` (a downward jump snaps — range switch). */
  setTarget(n: number): void;
  /** Jump immediately to `n` with no roll (range switch / first paint). */
  snapTo(n: number): void;
  /** Alias of snapTo, kept for callers that want a non-rolling set. */
  setValue(n: number): void;
  /** The latest value passed to setTarget / snapTo. */
  value(): number;
}

/** Normalize to a non-negative finite integer. */
function sanitize(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const r = Math.round(n);
  return r < 0 ? 0 : r;
}

/**
 * Create a continuous rolling number. Mount `handle.el`, then call `setTarget(n)`
 * to roll toward a value or `snapTo(n)` to jump (use snapTo for the first paint).
 */
export function createOdometer(_opts: OdometerOptions = {}): OdometerHandle {
  const root = document.createElement("span");
  root.className = "odometer";
  root.setAttribute("role", "text");

  let displayed = 0; // the value currently painted (a float, eased)
  let latest = 0; // most recent target value (returned by value())
  let frame = 0; // active rAF handle (0 == not scheduled)
  let lastTime: number | null = null; // previous frame timestamp for dt

  /** Paint the current `displayed` value as one cohesive formatted number. */
  function render(): void {
    const text = formatInt(displayed);
    root.textContent = text;
    root.dataset.value = text;
    root.setAttribute("aria-label", text);
  }

  function stop(): void {
    if (frame && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(frame);
    }
    frame = 0;
    lastTime = null;
  }

  /**
   * One frame: ease `displayed` toward `latest` on a slow exponential approach,
   * render, and reschedule until it converges (then stop so the rAF test stub
   * terminates). The slow time-constant keeps a single change rolling for
   * several seconds, so during active use the number rolls continuously.
   */
  function step(now: number): void {
    const dt = lastTime === null ? NOMINAL_FRAME_MS : Math.max(0, now - lastTime);
    lastTime = now;

    const gap = latest - displayed;
    if (gap < CONVERGE_EPSILON) {
      displayed = latest;
      render();
      stop();
      return;
    }

    // Exponential approach: close a TAU-scaled fraction of the gap each frame,
    // but never slower than MIN_UNITS_PER_SEC so the tail still visibly rolls.
    let move = gap * (1 - Math.exp(-dt / TAU_MS));
    const minMove = MIN_UNITS_PER_SEC * (dt / 1000);
    if (move < minMove) move = minMove;
    if (move > gap) move = gap;

    displayed += move;
    render();

    if (typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(step);
    } else {
      displayed = latest; // no rAF (tests): jump to the target
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
    // Downward target (range switch to a smaller total) snaps; don't roll down.
    if (v < displayed) {
      snapTo(v);
      return;
    }
    latest = v;
    start();
  }

  function value(): number {
    return latest;
  }

  render();
  return { el: root, setTarget, snapTo, setValue: snapTo, value };
}
