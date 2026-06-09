// Live token-total creep controller (pure, clock-injectable).
//
// The big "Total tokens · live" reel must keep rolling continuously while ANY
// session is active — but the real token total only changes when a message
// completes (per-message granularity), so there are multi-second gaps with no
// new data. This controller fabricates motion in those gaps:
//
//   • It estimates throughput (tokens/sec) from real updates.
//   • While active it advances a projected "override" total at that rate, with a
//     small velocity FLOOR so it never fully stops, and a decay that slows the
//     creep the further it leads the last real total (so it never shows a
//     wildly-wrong number — predict & reconcile).
//   • On each real update it reconciles: a higher real total pulls the override
//     up (raise-only, matching the odometer driver); a lower one is absorbed.
//   • When no session is active it reconciles down to the exact real total.
//
// It is PURE: no timers, no DOM, no rAF. The host (main.ts heartbeat) calls
// `observeReal` + `setActive` with the wall clock and feeds `tick()`'s result to
// the ticker's `totalOverride`. This keeps it unit-testable with a fake clock
// and leaves the odometer/ticker internals untouched.

export interface LiveCreepConfig {
  /** Floor velocity while active (tokens/sec). The reel never stops below this. */
  minTokensPerSec: number;
  /** Upper clamp on the estimated throughput (tokens/sec). */
  maxTokensPerSec: number;
  /** Lead (tokens ahead of the last real total) at which the rate-driven part of
   *  the velocity halves. Smaller = stays closer to truth. */
  leadHalfLifeTokens: number;
  /** EMA smoothing for the throughput estimate (0..1; higher = more reactive). */
  rateAlpha: number;
}

export const DEFAULT_LIVE_CREEP: LiveCreepConfig = {
  minTokensPerSec: 40,
  maxTokensPerSec: 8000,
  leadHalfLifeTokens: 2000,
  rateAlpha: 0.4,
};

function clampNonNegInt(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const r = Math.round(n);
  return r < 0 ? 0 : r;
}

export class LiveCreep {
  private readonly cfg: LiveCreepConfig;

  private lastReal = 0; // most recent real total observed
  private override = 0; // current projected display value (>= lastReal)
  private rate = 0; // smoothed throughput, tokens/sec
  private active = false;

  private prevReal = 0; // previous real total (for rate delta)
  private prevRealMs: number | null = null;
  private lastTickMs: number | null = null;

  constructor(cfg: LiveCreepConfig = DEFAULT_LIVE_CREEP) {
    this.cfg = cfg;
  }

  /** Record a fresh REAL total (from a heartbeat/SSE fetch) at `nowMs`. */
  observeReal(realTotal: number, nowMs: number): void {
    const r = clampNonNegInt(realTotal);

    if (r > this.lastReal) {
      // A real increment arrived: refresh the throughput estimate.
      if (this.prevRealMs !== null) {
        const dtSec = (nowMs - this.prevRealMs) / 1000;
        if (dtSec > 0) {
          const observed = (r - this.prevReal) / dtSec;
          // First sample seeds the rate; later samples smooth via EMA.
          this.rate =
            this.rate === 0
              ? observed
              : this.rate + this.cfg.rateAlpha * (observed - this.rate);
        }
      }
      this.prevReal = r;
      this.prevRealMs = nowMs;
      this.lastReal = r;
      // Raise-only reconcile: pull the display up to truth if it lagged.
      if (r > this.override) this.override = r;
    } else if (r > this.override) {
      // Defensive: never let the display sit below a known real total.
      this.override = r;
      this.lastReal = Math.max(this.lastReal, r);
    }
  }

  /** Set whether any session is currently active (working/thinking/responding). */
  setActive(active: boolean): void {
    this.active = active;
  }

  /** Restart at a known real total (e.g. on a range switch), clearing the lead
   *  and rate so the creep doesn't carry the previous range's projection. */
  reset(realTotal: number, nowMs: number): void {
    const r = clampNonNegInt(realTotal);
    this.lastReal = r;
    this.override = r;
    this.prevReal = r;
    this.prevRealMs = nowMs;
    this.rate = 0;
    this.lastTickMs = null;
  }

  /** Advance the projection to `nowMs` and return the value to display. */
  tick(nowMs: number): number {
    const dtSec =
      this.lastTickMs === null ? 0 : Math.max(0, (nowMs - this.lastTickMs) / 1000);
    this.lastTickMs = nowMs;

    if (!this.active) {
      // Idle: reconcile down to the exact real total and hold there.
      this.override = this.lastReal;
      return this.override;
    }

    const lead = Math.max(0, this.override - this.lastReal);
    const decay =
      this.cfg.leadHalfLifeTokens > 0
        ? Math.pow(0.5, lead / this.cfg.leadHalfLifeTokens)
        : 1;
    const base = Math.min(this.cfg.maxTokensPerSec, Math.max(0, this.rate));
    // Floor guarantees motion (never stops while active); decay keeps the lead
    // from running away when the rate is high but updates have paused.
    const velocity = Math.max(this.cfg.minTokensPerSec, base * decay);

    this.override += velocity * dtSec;
    return this.override;
  }

  /** Current projected display value. */
  value(): number {
    return this.override;
  }

  /** Whether the controller currently considers a session active. */
  isActive(): boolean {
    return this.active;
  }

  /** Smoothed throughput estimate (tokens/sec) — exposed for tests/telemetry. */
  ratePerSec(): number {
    return this.rate;
  }
}
