// Silky number-tick animation: tween an element's text from one value to the
// next over a short duration using requestAnimationFrame + easeOutCubic.
//
// Each call cancels any in-flight tween on the same element, so rapid updates
// retarget smoothly instead of stacking. Callers supply a `format(v)` callback
// so the tween renders tokens, costs, percentages, etc. with the correct
// formatter on every frame.
//
// NOTE: we deliberately do NOT honor `prefers-reduced-motion` here. On Windows
// with "animation effects" disabled, WebView2 reports reduced motion, which
// would otherwise snap every count instantly — the user wants the silky tick
// regardless of that OS setting. Each frame mutates textContent, which forces
// WebView2 to repaint, so this animates in the real app.

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
 * Cancels any previous tween on the same element. Snaps instantly when the
 * value is unchanged or rAF is unavailable (the latter keeps test envs
 * deterministic).
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

  if (start === end || typeof requestAnimationFrame !== "function") {
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
