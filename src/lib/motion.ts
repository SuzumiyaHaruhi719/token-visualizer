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
