const HEIGHT_TRANSITION_MS = 360;
const HEIGHT_CLEANUP_MS = HEIGHT_TRANSITION_MS + 100;

/** Run one DOM replacement while animating the container's old height to the new height. */
export function animateHeightChange(container: HTMLElement, mutate: () => void): void {
  if (typeof requestAnimationFrame !== "function" || !container.isConnected) {
    mutate();
    return;
  }

  const before = container.getBoundingClientRect().height;
  mutate();
  const after = container.getBoundingClientRect().height;
  if (!Number.isFinite(before) || !Number.isFinite(after) || Math.abs(before - after) < 1) return;

  const previousHeight = container.style.height;
  const previousOverflow = container.style.overflow;
  const previousTransition = container.style.transition;

  container.style.height = `${before}px`;
  container.style.overflow = "hidden";
  container.style.transition = "none";
  void container.offsetHeight;

  let finished = false;
  const cleanup = (): void => {
    if (finished) return;
    finished = true;
    container.removeEventListener("transitionend", onTransitionEnd);
    container.style.height = previousHeight;
    container.style.overflow = previousOverflow;
    container.style.transition = previousTransition;
  };
  const onTransitionEnd = (event: TransitionEvent): void => {
    if (event.target === container && event.propertyName === "height") cleanup();
  };

  container.addEventListener("transitionend", onTransitionEnd);
  requestAnimationFrame(() => {
    container.style.transition = `height ${HEIGHT_TRANSITION_MS}ms var(--ease-premium)`;
    container.style.height = `${after}px`;
    window.setTimeout(cleanup, HEIGHT_CLEANUP_MS);
  });
}

/** Remove an entry class on the next paint so CSS can animate from its start state. */
export function revealOnNextFrame(element: Element, className: string): void {
  if (typeof requestAnimationFrame !== "function") {
    element.classList.remove(className);
    return;
  }
  requestAnimationFrame(() => element.classList.remove(className));
}

// --- block content-swap transition (range tab switch) -----------------------
// Major dashboard blocks (KPI cluster, by-source split, the ECharts panels, the
// limits panel) cross-fade on a RANGE switch: the old content fades + lifts away,
// the content is swapped once, then the new content fades + rises in. This reads
// unmistakably as a transition (the user's repeated request) while staying a
// pair of one-shot CSS transitions (no perpetual rAF — WebView2-safe).
//
// REDUCED MOTION: we DO NOT skip the swap when the OS asks for reduced motion.
// Windows reports `prefers-reduced-motion: reduce` whenever the "Show
// animations"/"Animate controls" setting is OFF (very common), which previously
// made the whole effect invisible — the user reported "nothing happens" 3+
// times. The decisive fix: under reduced motion we still run the swap, but a
// gentle OPACITY-ONLY fade (no translate/scale/filter) via the `reduce` flag, so
// every user sees a clear transition regardless of that OS setting. The CSS for
// `.content-swap-*.is-reduced` keeps opacity but drops transform/filter.
//
// CRITICAL: only call this from the range-switch path (loadRange), never from
// the heartbeat / SSE refreshers — those run continuously and would strobe.

// Durations are intentionally generous so the dip is UNMISTAKABLE: ~210ms out,
// ~270ms in. The CSS drives opacity 1->0->1 with a ~16px translateY so the
// blocks visibly fade + slide on every range-tab click.
const SWAP_OUT_MS = 210;
const SWAP_ENTER_MS = 270;
const SWAP_CLEANUP_PAD_MS = 140;
// Light per-block stagger (ms) so the blocks ripple rather than snap in unison.
// Applied as a transition-delay via the `--swap-stagger` custom property.
const SWAP_STAGGER_STEP_MS = 36;
const SWAP_STAGGER_MAX_MS = 150;

const OUT_CLASS = "content-swap-out";
const ENTER_CLASS = "content-swap-enter";
const ACTIVE_CLASS = "content-swap-active";
// Marks a swap/reveal as the gentle reduced-motion variant (opacity only).
const REDUCED_CLASS = "is-reduced";

// Per-element generation stamp. A new swap (or reveal) on an element bumps its
// generation; any older swap still running checks the stamp before touching the
// element so its deferred cleanup can't strip classes off the newer swap (which
// would briefly re-enable pointer events / flash the dim during rapid clicks).
const swapGeneration = new WeakMap<HTMLElement, number>();

function bumpGeneration(targets: HTMLElement[]): number {
  const gen = (swapGeneration.get(targets[0]) ?? 0) + 1;
  for (const t of targets) swapGeneration.set(t, gen);
  return gen;
}

/** True only if EVERY target still belongs to this swap's generation. */
function ownsGeneration(targets: HTMLElement[], gen: number): boolean {
  return targets.every((t) => swapGeneration.get(t) === gen);
}

export interface ContentSwapOptions {
  /** Class that dims/lifts the OLD content during phase 1. */
  outClass?: string;
  /** Class that the NEW content starts in (removed next frame to animate in). */
  enterClass?: string;
  /** Marker class kept on elements for the duration (disables pointer events). */
  activeClass?: string;
  /** Phase-1 (fade-out) duration in ms. */
  outMs?: number;
  /** Phase-2 (fade/rise-in) duration in ms. */
  enterMs?: number;
  /** Optional staleness guard: if it ever returns false the swap is abandoned. */
  isCurrent?: () => boolean;
}

/** True when the OS asks for reduced motion (mirrors the existing CSS gate). */
export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/** Normalize a loose element list to connected HTMLElements only. */
function connectedElements(
  elements: Iterable<Element | null | undefined>,
): HTMLElement[] {
  const out: HTMLElement[] = [];
  for (const node of elements) {
    if (node instanceof HTMLElement && node.isConnected) out.push(node);
  }
  return out;
}

/**
 * True when we cannot run a real transition at all — mutate inline. This is now
 * ONLY the structural cases (no targets / no rAF, i.e. jsdom + tests). Reduced
 * motion NO LONGER skips: it downgrades to an opacity-only fade instead (see the
 * module header), so the swap stays visible when Windows animations are off.
 */
function shouldSkipMotion(targets: HTMLElement[]): boolean {
  return targets.length === 0 || typeof requestAnimationFrame !== "function";
}

/** Stagger delay (ms) for the i-th block, capped so long lists don't drag. */
function staggerDelayMs(index: number): number {
  return Math.min(index * SWAP_STAGGER_STEP_MS, SWAP_STAGGER_MAX_MS);
}

/** Set/clear the per-element transition-delay used for the entrance stagger. */
function setStagger(el: HTMLElement, index: number | null): void {
  if (index === null) el.style.removeProperty("--swap-stagger");
  else el.style.setProperty("--swap-stagger", `${staggerDelayMs(index)}ms`);
}

/**
 * Two-phase block transition around a single DOM mutation:
 *   1. fade + lift the OLD content (`outClass`) for `outMs`,
 *   2. run `mutate()` once,
 *   3. fade + rise the NEW content in (`enterClass`) over `enterMs`.
 *
 * Resolves to `true` iff the mutation ran. Under REDUCED MOTION it still runs
 * (async) as a gentle opacity-only fade — it does NOT skip, so the swap stays
 * visible when Windows animations are off. It only mutates immediately +
 * resolves synchronously for the structural cases (no targets / no rAF =
 * jsdom/tests). The `isCurrent` guard lets a newer range switch abandon a stale
 * in-flight transition before it mutates (transient classes are cleaned up).
 */
export function transitionContentSwap(
  elements: Iterable<Element | null | undefined>,
  mutate: () => void,
  options: ContentSwapOptions = {},
): Promise<boolean> {
  const {
    outClass = OUT_CLASS,
    enterClass = ENTER_CLASS,
    activeClass = ACTIVE_CLASS,
    outMs = SWAP_OUT_MS,
    enterMs = SWAP_ENTER_MS,
    isCurrent,
  } = options;

  const targets = connectedElements(elements);

  // Fast, deterministic path for tests only (no targets / no rAF). Reduced
  // motion does NOT take this path anymore — it still animates (opacity only).
  if (shouldSkipMotion(targets)) {
    if (isCurrent && !isCurrent()) return Promise.resolve(false);
    mutate();
    return Promise.resolve(true);
  }

  // Under reduced motion run a gentle OPACITY-ONLY variant (still visible) so the
  // OS "animations off" setting can't make the swap disappear entirely.
  const reduced = prefersReducedMotion();

  // Claim this generation; an interrupting swap will bump it and we'll bail.
  const gen = bumpGeneration(targets);

  // Clear any classes left by an interrupted prior swap so we start clean.
  for (const t of targets) t.classList.remove(outClass, enterClass, REDUCED_CLASS);

  // Phase 1: fade + lift the old content (opacity-only when reduced). Stagger the
  // blocks so they ripple rather than move in lockstep.
  targets.forEach((t, i) => {
    if (reduced) t.classList.add(REDUCED_CLASS);
    setStagger(t, i);
    t.classList.add(activeClass, outClass);
  });

  /** Strip every transient swap class + stagger from targets we still own. */
  const cleanupOwned = (): void => {
    if (!ownsGeneration(targets, gen)) return; // a newer swap owns them — leave its classes
    for (const t of targets) {
      t.classList.remove(outClass, enterClass, activeClass, REDUCED_CLASS);
      setStagger(t, null);
    }
  };

  return new Promise<boolean>((resolve) => {
    window.setTimeout(() => {
      // Abandon if superseded. Two cases differ in ownership:
      //   • a newer SWAP bumped the generation → it owns the classes, don't touch.
      //   • a newer FETCH flipped isCurrent() but no newer swap ran (e.g. a
      //     same-range loadRange that takes the no-swap branch) → WE still own the
      //     targets, so we must clean up or they stay faded + pointer-events:none.
      if ((isCurrent && !isCurrent()) || !ownsGeneration(targets, gen)) {
        cleanupOwned();
        resolve(false);
        return;
      }

      // Swap content once, then prime the enter state (still hidden/lifted).
      mutate();
      for (const t of targets) {
        t.classList.remove(outClass);
        t.classList.add(enterClass);
      }
      void targets[0].offsetHeight; // force a style flush so the enter is a real start state

      // Phase 2: reveal next frame so the swapped (and, for charts, repainted)
      // content fades + rises in rather than popping.
      requestAnimationFrame(() => {
        if (!ownsGeneration(targets, gen)) {
          resolve(false);
          return;
        }
        for (const t of targets) t.classList.remove(enterClass);
        window.setTimeout(() => {
          cleanupOwned();
          resolve(true);
        }, enterMs + SWAP_CLEANUP_PAD_MS);
      });
    }, outMs);
  });
}

/**
 * First-mount entrance: ease the given blocks in from the enter state. Reuses
 * the same `content-swap-enter` class + reveal-next-frame trick + the staggered
 * ripple. No fade-out phase (there is no prior content). Under reduced motion it
 * runs the gentle opacity-only variant; only structural cases (no targets / no
 * rAF) no-op.
 */
export function revealContent(
  elements: Iterable<Element | null | undefined>,
  options: Pick<ContentSwapOptions, "enterClass"> = {},
): boolean {
  const { enterClass = ENTER_CLASS } = options;
  const targets = connectedElements(elements);
  if (shouldSkipMotion(targets)) return false;

  const reduced = prefersReducedMotion();
  const gen = bumpGeneration(targets);
  targets.forEach((t, i) => {
    if (reduced) t.classList.add(REDUCED_CLASS);
    setStagger(t, i);
    t.classList.add(enterClass);
  });
  void targets[0].offsetHeight; // establish the start state before revealing
  requestAnimationFrame(() => {
    if (!ownsGeneration(targets, gen)) return;
    for (const t of targets) t.classList.remove(enterClass);
    window.setTimeout(() => {
      if (!ownsGeneration(targets, gen)) return;
      for (const t of targets) {
        t.classList.remove(REDUCED_CLASS);
        setStagger(t, null);
      }
    }, SWAP_ENTER_MS + SWAP_STAGGER_MAX_MS + SWAP_CLEANUP_PAD_MS);
  });
  return true;
}

// --- relative freshness ("Ns ago") -----------------------------------------
// Render an epoch-ms timestamp as a short relative-age label for the session
// rows, refreshed live on the heartbeat tick. The reference clawd UI shows a
// freshness like "1m 49s ago"; we use a compact single-unit form that stays
// readable at the dashboard AND the tiny popover font.

const FRESH_JUST_NOW_MS = 10_000; // < 10s reads as "just now"
const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

/**
 * Compact relative age of `updatedAtMs` (epoch ms) vs `nowMs`:
 *   < 10s -> "just now", < 60s -> "Ns ago", < 60m -> "Nm ago",
 *   < 24h -> "Nh ago", else "Nd ago". Non-finite / future input -> "just now".
 */
export function formatFreshness(
  updatedAtMs: number | null | undefined,
  nowMs: number = Date.now(),
): string {
  if (updatedAtMs === null || updatedAtMs === undefined || !Number.isFinite(updatedAtMs)) {
    return "just now";
  }
  const diff = nowMs - updatedAtMs;
  if (diff < FRESH_JUST_NOW_MS) return "just now"; // also covers small clock skew (future)
  if (diff < MINUTE_MS) return `${Math.floor(diff / 1000)}s ago`;
  if (diff < HOUR_MS) return `${Math.floor(diff / MINUTE_MS)}m ago`;
  if (diff < DAY_MS) return `${Math.floor(diff / HOUR_MS)}h ago`;
  return `${Math.floor(diff / DAY_MS)}d ago`;
}

// --- session source icon ----------------------------------------------------
// A small inline SVG glyph that distinguishes the live session's agent, tinted by
// the source accent (coral for Claude, blue for Codex, violet for DeepSeek) via
// `currentColor` on the class. No external assets. Abstract marks: an
// asterisk/sunburst for Claude, a hex-knot for Codex, and a stacked-chevron
// "dive" mark for DeepSeek (per the design pass).

type SessionSource = "claude" | "codex" | "deepseek";

const SOURCE_ICON_PATHS: Record<SessionSource, string> = {
  claude:
    'M8 1.8v12.4M1.8 8h12.4M3.6 3.6l8.8 8.8M12.4 3.6l-8.8 8.8',
  codex:
    'M8 1.9 13.3 5v6L8 14.1 2.7 11V5L8 1.9Zm0 2.5L5 6.1v3.8l3 1.7 3-1.7V6.1L8 4.4Zm0 0v7.2M5 6.1l6 3.8M11 6.1 5 9.9',
  // Three nested downward chevrons — a "deep dive" mark, evoking depth/seek.
  deepseek: 'M3 4.2 8 8l5-3.8M3 7.6 8 11.4l5-3.8M3 11l5 3.8 5-3.8',
};

const STROKE_WIDTH: Record<SessionSource, number> = {
  claude: 1.65,
  codex: 1.25,
  deepseek: 1.4,
};

/** Sources whose glyph reads better with rounded line joins. */
const ROUND_JOIN: ReadonlySet<SessionSource> = new Set<SessionSource>(["codex", "deepseek"]);

/**
 * Inline SVG markup for a session's source glyph. `extraClass` lets each view
 * add its own sizing class (`.cs-source-icon` / `.pop-sess-icon`); the
 * source-specific modifier carries the accent color. `stroke="currentColor"`
 * keeps it monochrome + themeable.
 */
export function sourceIconSvg(source: SessionSource, extraClass: string): string {
  const strokeWidth = STROKE_WIDTH[source];
  const join = ROUND_JOIN.has(source) ? ' stroke-linejoin="round"' : "";
  return (
    `<svg class="${extraClass} ${extraClass}--${source}" viewBox="0 0 16 16" ` +
    `aria-hidden="true" focusable="false">` +
    `<path d="${SOURCE_ICON_PATHS[source]}" fill="none" stroke="currentColor" ` +
    `stroke-width="${strokeWidth}" stroke-linecap="round"${join}/></svg>`
  );
}
