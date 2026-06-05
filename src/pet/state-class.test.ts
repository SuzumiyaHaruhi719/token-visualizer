import { describe, it, expect } from "vitest";
import { stateToClass, stateKind } from "./Clawd";
import type { PetState } from "../lib/types";

describe("stateToClass", () => {
  const cases: { state: PetState; expected: string }[] = [
    { state: { kind: "idle" }, expected: "state-idle" },
    { state: { kind: "thinking" }, expected: "state-thinking" },
    { state: { kind: "responding" }, expected: "state-responding" },
    { state: { kind: "waiting" }, expected: "state-waiting" },
    { state: { kind: "sleeping" }, expected: "state-sleeping" },
    { state: { kind: "working", tool: "Bash" }, expected: "state-working" },
    { state: { kind: "working", tool: null }, expected: "state-working" },
  ];

  for (const { state, expected } of cases) {
    it(`maps ${JSON.stringify(state)} -> ${expected}`, () => {
      expect(stateToClass(state)).toBe(expected);
    });
  }

  it("covers every PetState kind", () => {
    const kinds = new Set(cases.map((c) => c.state.kind));
    expect(kinds).toEqual(
      new Set(["idle", "thinking", "responding", "waiting", "sleeping", "working"]),
    );
  });

  it("stateKind returns the discriminant", () => {
    expect(stateKind({ kind: "working", tool: "Edit" })).toBe("working");
    expect(stateKind({ kind: "idle" })).toBe("idle");
  });
});
