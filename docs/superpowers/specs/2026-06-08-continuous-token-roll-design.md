# Continuous "live" token roll — design

## Problem

The big **"Total tokens · live"** number stops rolling between real token updates.
Requirement (stated repeatedly by the user): **as long as ≥1 session is active, the
number must keep rolling continuously — it must never freeze.**

## Root cause

`src/components/odometer.ts` eases `displayed` toward `latest` and calls `stop()`
the instant `gap < CONVERGE_EPSILON` (killing the rAF). Real token totals only
change when a message *completes*, so between completions — and during a long
in-progress generation — there is no higher target, the reel converges, and it
freezes. The stop is deliberate in the current code: an earlier always-on creep
fed the reel an ever-rising target so its rAF ran forever, and combined with a
per-frame `filter: drop-shadow` it once saturated WebView2 and froze the whole
dashboard. So perpetual motion was removed.

## Approach

A small **live-creep controller in `src/main.ts`** that feeds the EXISTING
`updateTokenTicker(container, summary, mode, totalOverride?)` hook — built (per
its own doc comment) to "drive a perpetually-creeping total while the per-model
rows track their real values." **No changes to `odometer.ts` or
`token-ticker.ts` internals** (the protected 底层).

Rejected alternatives: bake a velocity floor into the odometer driver (touches
the protected internals); a separate live-counter model layer (overkill, YAGNI).

## Behavior

1. **Active gate.** Creep runs only while ≥1 session is in an active state
   (working / thinking / responding). Depends on the parallel session-state fix
   so that *reasoning counts as thinking* — otherwise the roll would die during a
   reasoning pause (the "reasoning → idle" bug).
2. **Rate estimate.** On each heartbeat where the real total grows, fold
   `Δtokens / Δt` into a smoothed tokens/sec estimate (the creep speed), clamped
   to a sane range.
3. **Continuous creep.** While active, project
   `total = lastRealTotal + rate × elapsedSinceLastReal` and pass it as
   `totalOverride`. A **small minimum velocity floor** guarantees visible motion
   even when the real rate is ~0 — small enough that the units reel keeps rolling
   while the lead stays negligible against a multi-million/billion total (tens of
   tokens/sec → a long gap accrues only a few hundred tokens of lead). The big
   reel rolls nonstop; per-model rows keep their EXACT real values (override only
   affects the headline total).
4. **Predict & reconcile** (the user's chosen accuracy stance). On each real
   update, reset `lastRealTotal` / time and re-estimate the rate. `setTarget` is
   raise-only — a real total below the crept display is absorbed (no backward
   jerk); a higher one pulls it up. The creep velocity **decays toward the small
   floor the further it leads** `lastRealTotal`: it slows as the lead grows (so it
   never shows a wildly-wrong number) but never drops below the floor (so it never
   fully stops while a session is active). "Never stops" wins over "zero lead" in
   the conflict — the floor is small enough that the lead is negligible in
   practice and reconciles on the next real update.
5. **Stop when idle.** When no session is active, drop the override → the reel
   eases to the real total and stops, freeing the event loop (preserves the perf
   fix that cured the old freeze).

## Performance

A continuous rAF while active is acceptable now that the per-frame
`filter: drop-shadow` (the actual cause of the old whole-app freeze) is gone,
replaced by a static `::before` glow. The creep is bounded to active periods
only. Before shipping, confirm the reel's `mask-image` + glow compositing stays
cheap under sustained motion (no per-frame re-rasterization).

## Testing

- **Unit (vitest):** extract the creep math into a pure, clock-injectable
  controller. Tests: (a) active + no new real total still advances the override
  over simulated time; (b) active=false converges to the real total and holds;
  (c) a real update higher than the creep pulls the target up; (d) the creep
  velocity decays as the lead grows but never drops below the small floor — over
  a long gap the override keeps advancing at ~the floor rate (negligible lead vs
  the total), reconciling on the next real update; (e) rate estimate tracks
  observed throughput.
- Keep existing odometer + token-ticker tests green (no internal changes).
- **Manual:** with an active session, the reel visibly rolls between message
  completions and never freezes; when all sessions go idle it settles to the
  exact total.

## Out of scope

Per-model row creep (rows stay exact); any change to the odometer reel rendering
or the ease driver.
