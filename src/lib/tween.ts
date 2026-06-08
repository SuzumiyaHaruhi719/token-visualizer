// Silky number-tick animation: tween an element's text from one value to the
// next over a short duration using requestAnimationFrame + easeOutCubic.
//
// Each call cancels any in-flight tween on the same element, so rapid updates
// retarget smoothly instead of stacking. A `prefers-reduced-motion` guard snaps
// instantly. Callers supply a `format(v)` callback so the tween renders tokens,
// costs, percentages, etc. with the correct formatter on every frame.

const DEFAULT_DURATION_MS = 700;

export interface AnimateNumberOptions {
  /** How long the tween runs, in ms. Defaults to ~700ms. */
  durationMs?: number;
  /** Maps the in-flight numeric value to display text each frame. */
  format: (value: number) => string;
}

/** Cubic ease-out: fast start, gentle settle. */
function easeOutCubic(t: number): number {
  const clamped = t < 0 ? 0 : t > 1 ? 1 : t;
  return 1 - Math.pow(1 - clamped, 3);
}

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

// Per-element handle to the active animation frame so a new tween can cancel it.
const activeFrames = new WeakMap<HTMLElement, number>();

function cancelActive(el: HTMLElement): void {
  const handle = activeFrames.get(el);
  if (handle !== undefined && typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(handle);
  }
  activeFrames.delete(el);
}

/**
 * Tween `el.textContent` from `from` to `to`, formatting each frame.
 * Cancels any previous tween on the same element. Snaps instantly when
 * reduced motion is requested or rAF is unavailable (e.g. jsdom/tests).
 */
export function animateNumber(
  el: HTMLElement,
  from: number,
  to: number,
  opts: AnimateNumberOptions,
): void {
  const { format } = opts;
  cancelActive(el);

  const start = Number.isFinite(from) ? from : 0;
  const end = Number.isFinite(to) ? to : 0;

  if (
    start === end ||
    prefersReducedMotion() ||
    typeof requestAnimationFrame !== "function"
  ) {
    el.textContent = format(end);
    return;
  }

  const duration = opts.durationMs ?? DEFAULT_DURATION_MS;
  const delta = end - start;
  let startTime: number | null = null;

  const step = (now: number): void => {
    if (startTime === null) startTime = now;
    const elapsed = now - startTime;
    const progress = duration > 0 ? elapsed / duration : 1;
    const eased = easeOutCubic(progress);
    const value = start + delta * eased;
    el.textContent = format(value);

    if (progress < 1) {
      activeFrames.set(el, requestAnimationFrame(step));
    } else {
      el.textContent = format(end);
      activeFrames.delete(el);
    }
  };

  activeFrames.set(el, requestAnimationFrame(step));
}
