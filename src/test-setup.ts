// Global vitest setup: make the number tweens settle synchronously.
//
// `animateNumber` (src/lib/tween.ts) drives its count-up via
// requestAnimationFrame. Vitest's jsdom env polyfills rAF on a real timer, so
// without help the tween text would still be mid-flight right after a render
// and assertions on final values would flake. We replace rAF with a stub that
// invokes the callback immediately, advancing a monotonic clock by a huge step
// each call so the SECOND frame's timestamp is already past any tween duration
// — the tween reaches progress >= 1 on that frame and jumps to its final
// formatted value (no unbounded recursion). Tests that need to observe
// intermediate frames (tween.test.ts) install their own queue-based stub via
// vi.stubGlobal, which transparently overrides this one.
import { beforeEach } from "vitest";

const TIMESTAMP_STEP_MS = 1e9;

beforeEach(() => {
  let clock = 0;
  globalThis.requestAnimationFrame = ((cb: FrameRequestCallback): number => {
    clock += TIMESTAMP_STEP_MS;
    cb(clock);
    return 0;
  }) as typeof requestAnimationFrame;
  globalThis.cancelAnimationFrame = (() => {}) as typeof cancelAnimationFrame;
});
