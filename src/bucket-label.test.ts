// Regression test for the "Token usage over time" today-bucket timezone bug.
//
// The backend bakes the user's LOCAL wall-clock hour into the bucket string but
// tags it with a misleading "Z" (a 01:00-local event becomes "...T01:00:00Z").
// `bucketLabel` must render that baked hour AS-IS for range="today" (via
// timeZone:"UTC") instead of re-localizing it — otherwise the local offset is
// applied a SECOND time (01 -> 09 at UTC+8).
//
// We force a non-UTC machine timezone (UTC+8) BEFORE importing the module so the
// double-offset bug, if present, would surface as "09".

// Force a non-UTC machine timezone BEFORE importing main.ts so the double-offset
// bug, if present, surfaces. `@types/node` isn't installed, so reach `process`
// via globalThis with a narrow local cast (vitest provides it at runtime).
(globalThis as { process?: { env: Record<string, string | undefined> } }).process!.env.TZ =
  "Asia/Shanghai"; // UTC+8, no DST — deterministic

import { describe, it, expect, vi } from "vitest";

// main.ts imports echarts (canvas-backed); stub it so importing the module in
// jsdom is side-effect free, matching the dashboard tests.
vi.mock("echarts", () => ({
  init: () => ({ setOption: () => {}, resize: () => {}, dispose: () => {} }),
  graphic: {
    LinearGradient: class {
      constructor(
        public x: number,
        public y: number,
        public x2: number,
        public y2: number,
        public colorStops: { offset: number; color: string }[],
      ) {}
    },
  },
}));

import { bucketLabel } from "./main";

describe("bucketLabel (today timezone)", () => {
  it("renders the baked local hour as-is, NOT re-localized, at UTC+8", () => {
    // The backend already baked 01:00 LOCAL into this string. Even on a UTC+8
    // machine the label must read the 01 hour, never 09.
    const label = bucketLabel("2026-06-09T01:00:00Z", "today");
    expect(label).toMatch(/\b01\b/);
    expect(label).not.toMatch(/\b09\b/);
  });

  it("renders midnight as the 12/00 hour, not shifted by the offset", () => {
    // 00:00 baked-local must not become 08:00 (UTC+8 double offset).
    const label = bucketLabel("2026-06-09T00:00:00Z", "today");
    expect(label).not.toMatch(/\b08\b/);
    // 24h locales show "00"; 12h locales show "12" (midnight).
    expect(label).toMatch(/\b00\b|\b12\b/);
  });

  it("keeps daily buckets on the date label for non-today ranges", () => {
    // Non-today branch is unchanged: a date label (month + day), not an hour.
    const label = bucketLabel("2026-06-09T00:00:00Z", "30d");
    expect(label).toMatch(/Jun/);
  });

  it("returns the raw string for an unparseable bucket", () => {
    expect(bucketLabel("not-a-date", "today")).toBe("not-a-date");
  });
});
