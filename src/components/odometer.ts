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

// Playback lag: a bit above the dashboard's ~500ms sample cadence so a "next"
// sample is always buffered to interpolate toward (=> continuous motion).
const RENDER_DELAY_MS = 750;

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
  let latest = 0; // most recent value pushed (returned by value())
  // Recent samples in ASCENDING time order; the driver plays back through them
  // RENDER_DELAY_MS behind real time. Timestamps share `clock()` with rAF.
  let samples: { v: number; t: number }[] = [];
  let frame = 0; // active rAF handle (0 == not scheduled)

  /** Monotonic clock shared by sample timestamps and the rAF callback. */
  function clock(): number {
    return typeof performance !== "undefined" && typeof performance.now === "function"
      ? performance.now()
      : Date.now();
  }

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
  }

  /** Drop samples older than the one bracketing the current render time. */
  function trimSamples(renderTime: number): void {
    let keepFrom = 0;
    for (let i = 0; i < samples.length - 1; i++) {
      if (samples[i + 1].t <= renderTime) keepFrom = i + 1;
    }
    if (keepFrom > 0) samples = samples.slice(keepFrom);
  }

  /**
   * One playback frame: interpolate `displayed` between the two buffered samples
   * bracketing `now - RENDER_DELAY_MS`, render, and reschedule until playback has
   * drained to the latest sample (then stop so the rAF test stub terminates).
   */
  function step(now: number): void {
    const renderTime = now - RENDER_DELAY_MS;

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
      displayed = a.v;
    }
    render();
    trimSamples(renderTime);

    const last = samples[samples.length - 1];
    if (b !== null && renderTime < last.t && typeof requestAnimationFrame === "function") {
      frame = requestAnimationFrame(step);
    } else {
      // Drained: settle exactly on the latest value and stop scheduling.
      displayed = last.v;
      render();
      samples = [last];
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
    frame = requestAnimationFrame(step);
  }

  function snapTo(n: number): void {
    const v = sanitize(n);
    latest = v;
    displayed = v;
    samples = [];
    stop();
    render();
  }

  function setTarget(n: number): void {
    const v = sanitize(n);
    if (v < displayed) {
      snapTo(v);
      return;
    }
    latest = v;
    if (v === displayed && samples.length === 0) {
      render();
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

  render();
  return { el: root, setTarget, snapTo, setValue: snapTo, value };
}
