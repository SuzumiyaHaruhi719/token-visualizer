import { describe, it, expect } from "vitest";

import { LiveCreep, DEFAULT_LIVE_CREEP } from "./live-creep";

describe("LiveCreep", () => {
  it("advances continuously while active even with no new real total", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0);
    c.setActive(true);
    c.tick(0); // seed the clock
    const v1 = c.tick(1000); // +1s
    const v2 = c.tick(2000); // +2s
    expect(v1).toBeGreaterThan(1000);
    expect(v2).toBeGreaterThan(v1); // never stalls while active
    // floor velocity ~40 tok/s with no learned rate
    expect(v1).toBeCloseTo(1000 + DEFAULT_LIVE_CREEP.minTokensPerSec, 0);
  });

  it("reconciles down to the exact real total and holds when idle", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0);
    c.setActive(true);
    c.tick(0);
    c.tick(5000); // crept ahead of 1000
    expect(c.value()).toBeGreaterThan(1000);

    c.setActive(false);
    expect(c.tick(6000)).toBe(1000); // snaps back to truth
    expect(c.tick(10000)).toBe(1000); // and holds
  });

  it("pulls the display UP when a real total exceeds the crept value", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0);
    c.setActive(true);
    c.tick(0);
    c.tick(1000); // ~1040
    c.observeReal(5000, 1000); // real jump above the creep
    expect(c.value()).toBe(5000);
  });

  it("does NOT yank the display backward when a real total is below the creep", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0);
    c.setActive(true);
    c.tick(0);
    c.tick(5000);
    const ahead = c.value();
    expect(ahead).toBeGreaterThan(1100);

    c.observeReal(1100, 6000); // real below the crept value
    expect(c.value()).toBe(ahead); // absorbed, not yanked down (raise-only)
  });

  it("keeps moving but bounds the lead via decay over a long gap, then reconciles", () => {
    const c = new LiveCreep();
    c.observeReal(1_000_000, 0);
    c.observeReal(1_010_000, 1000); // learn a high rate (~10k tok/s)
    c.setActive(true);
    c.tick(1000); // seed

    let v = c.value();
    for (let t = 2000; t <= 60_000; t += 1000) {
      const next = c.tick(t);
      expect(next).toBeGreaterThanOrEqual(v); // monotonic — never stops
      v = next;
    }
    const lead = v - 1_010_000;
    // Decay keeps the lead far below an un-decayed run (8000 tok/s * ~59s ≈ 472k).
    expect(lead).toBeGreaterThan(0);
    expect(lead).toBeLessThan(50_000);

    // A real completion above the crept value reconciles cleanly.
    c.observeReal(1_100_000, 61_000);
    expect(c.value()).toBe(1_100_000);
  });

  it("estimates throughput from real updates (EMA)", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0); // first real: no rate yet
    c.observeReal(2000, 1000); // +1000 in 1s -> seed 1000/s
    expect(c.ratePerSec()).toBeCloseTo(1000, 0);
    c.observeReal(4000, 2000); // +2000 in 1s -> EMA 1000 + 0.4*(2000-1000) = 1400
    expect(c.ratePerSec()).toBeCloseTo(1400, 0);
  });

  it("reset() restarts at a new total and clears the lead", () => {
    const c = new LiveCreep();
    c.observeReal(1000, 0);
    c.setActive(true);
    c.tick(0);
    c.tick(5000); // crept ahead of 1000
    expect(c.value()).toBeGreaterThan(1000);

    c.reset(50, 6000); // e.g. switch from "all" to "today"
    expect(c.value()).toBe(50);
    expect(c.ratePerSec()).toBe(0);
    c.tick(6000); // seed clock at the new base
    expect(c.tick(7000)).toBeGreaterThan(50); // creeps forward from 50, not the old value
  });

  it("creeps at the floor when active with an unknown rate", () => {
    const c = new LiveCreep({ ...DEFAULT_LIVE_CREEP, minTokensPerSec: 100 });
    c.observeReal(500, 0);
    c.setActive(true);
    c.tick(0);
    expect(c.tick(1000)).toBeCloseTo(600, 0); // 500 + 100/s * 1s
  });
});
