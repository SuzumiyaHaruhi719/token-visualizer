import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { subscribe } from "./api";
import type { CmServerEvent } from "./types";

class FakeEventSource {
  static instances: FakeEventSource[] = [];

  onerror: ((event: Event) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;
  closed = false;
  listeners = new Map<string, Array<(event: MessageEvent) => void>>();

  constructor(public url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(name: string, handler: (event: MessageEvent) => void): void {
    const list = this.listeners.get(name) ?? [];
    list.push(handler);
    this.listeners.set(name, list);
  }

  close(): void {
    this.closed = true;
  }

  emit(name: string, data: string): void {
    for (const handler of this.listeners.get(name) ?? []) {
      handler({ data } as MessageEvent);
    }
  }

  fail(): void {
    this.onerror?.(new Event("error"));
  }
}

describe("subscribe", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeEventSource.instances = [];
    (window as any).__CM_PORT__ = 43123;
    (window as any).__CM_MOCK__ = false;
    (globalThis as any).EventSource = FakeEventSource;
  });

  afterEach(() => {
    vi.useRealTimers();
    delete (window as any).__CM_PORT__;
    delete (window as any).__CM_MOCK__;
    delete (globalThis as any).EventSource;
  });

  it("reconnects with exponential backoff after errors", () => {
    const stop = subscribe(() => {});
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.instances[0].url).toBe("http://127.0.0.1:43123/events");

    FakeEventSource.instances[0].fail();
    expect(FakeEventSource.instances[0].closed).toBe(true);
    vi.advanceTimersByTime(999);
    expect(FakeEventSource.instances).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.instances).toHaveLength(2);

    FakeEventSource.instances[1].fail();
    vi.advanceTimersByTime(1_999);
    expect(FakeEventSource.instances).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.instances).toHaveLength(3);

    stop();
    FakeEventSource.instances[2].fail();
    vi.advanceTimersByTime(30_000);
    expect(FakeEventSource.instances).toHaveLength(3);
    expect(FakeEventSource.instances[2].closed).toBe(true);
  });

  it("parses named events and ignores malformed frames", () => {
    const events: CmServerEvent[] = [];
    const stop = subscribe((event) => events.push(event));

    FakeEventSource.instances[0].emit("usage", JSON.stringify({ current: null }));
    FakeEventSource.instances[0].emit("usage", "{not json");

    expect(events).toEqual([{ type: "usage", data: { current: null } }]);
    stop();
  });
});
