// API client for the Claude Monitor backend (HTTP + SSE), with a standalone
// MOCK mode so the pages render and tests run without a backend.
//
// Base URL resolution (per the pinned contract):
//   1. window.__CM_PORT__ (number) -> http://127.0.0.1:<port>
//   2. <meta name="cm-port" content="..."> -> http://127.0.0.1:<port>
//   3. else same-origin (relative URLs)
// Host is ALWAYS 127.0.0.1 (never localhost) to dodge the proxy hazard.

import type {
  Summary,
  SessionState,
  RangeKey,
  CmServerEvent,
  Totals,
  Limits,
  AppSettings,
  AppSettingsPatch,
} from "./types";
import type { FxPayload } from "./currency";

declare global {
  interface Window {
    __CM_PORT__?: number | string;
    /** Force mock mode regardless of fetch availability (tests / standalone). */
    __CM_MOCK__?: boolean;
  }
}

const HOST = "127.0.0.1";

function readPort(): string | null {
  if (typeof window !== "undefined" && window.__CM_PORT__ != null) {
    return String(window.__CM_PORT__);
  }
  if (typeof document !== "undefined") {
    const meta = document.querySelector('meta[name="cm-port"]');
    const content = meta?.getAttribute("content");
    if (content) return content;
  }
  return null;
}

/** Absolute base URL for the backend, or "" for same-origin. */
export function baseUrl(): string {
  const port = readPort();
  return port ? `http://${HOST}:${port}` : "";
}

function apiUrl(path: string): string {
  return `${baseUrl()}${path}`;
}

/** Whether we should use mock data instead of hitting the network. */
export function isMockForced(): boolean {
  if (typeof window !== "undefined" && window.__CM_MOCK__) return true;
  // In Vite dev with no port hint and no backend, default to mock so the
  // pages are usable standalone. A real port/meta disables this.
  const hasBackendHint = readPort() !== null;
  const dev =
    typeof import.meta !== "undefined" && (import.meta as any).env?.DEV === true;
  return dev && !hasBackendHint;
}

// ---------------------------------------------------------------------------
// Mock builders (exported for tests + standalone rendering)
// ---------------------------------------------------------------------------

export function mockTotals(): Totals {
  const input = 12_400_000;
  const output = 3_100_000;
  const cacheCreate = 6_800_000;
  const cacheRead = 96_200_000;
  const tokens = input + output + cacheCreate + cacheRead;
  const cacheHitRate = cacheRead / (input + cacheCreate + cacheRead);
  return {
    tokens,
    input,
    output,
    cacheCreate,
    cacheRead,
    costUsd: 41.07,
    cacheHitRate,
    messages: 4821,
    sessions: 37,
  };
}

export function mockSummary(range: RangeKey = "today"): Summary {
  const buckets = range === "today" ? 24 : range === "7d" ? 7 : 30;
  const now = Date.now();
  const stepMs = range === "today" ? 3_600_000 : 86_400_000;
  const timeseries = Array.from({ length: buckets }, (_, i) => {
    const t = now - (buckets - 1 - i) * stepMs;
    const wave = 0.5 + 0.5 * Math.sin((i / buckets) * Math.PI * 2);
    return {
      bucket: new Date(t).toISOString(),
      input: Math.round(120_000 + wave * 380_000),
      output: Math.round(30_000 + wave * 90_000),
      cacheCreate: Math.round(60_000 + wave * 140_000),
      cacheRead: Math.round(900_000 + wave * 2_600_000),
    };
  });
  return {
    range,
    totals: mockTotals(),
    byModel: [
      { model: "claude-opus-4-8", tokens: 78_400_000, costUsd: 33.2 },
      { model: "claude-sonnet-4-6", tokens: 32_100_000, costUsd: 6.9 },
      { model: "claude-haiku-4-5", tokens: 8_000_000, costUsd: 0.97 },
    ],
    byProject: [
      { project: "claude-monitor", tokens: 54_300_000 },
      { project: "CorePilot", tokens: 38_900_000 },
      { project: "8111Reader", tokens: 14_200_000 },
      { project: "gstack", tokens: 7_600_000 },
      { project: "scratch", tokens: 3_500_000 },
    ],
    bySource: [
      { source: "claude", tokens: 108_400_000, costUsd: 39.9 },
      { source: "codex", tokens: 10_100_000, costUsd: 1.17 },
    ],
    timeseries,
  };
}

export function mockLimits(): Limits {
  const now = Math.floor(Date.now() / 1000);
  return {
    claude: {
      session: {
        project: "claude-monitor",
        model: "claude-opus-4-8",
        tokens: 1_240_000,
        state: { kind: "working", tool: "Edit" },
      },
      fiveHour: null,
      weekly: null,
      note: "remaining not exposed locally",
    },
    codex: {
      session: { model: "gpt-5.4-codex", tokens: 412_000 },
      fiveHour: { usedPercent: 38, remainingPercent: 62, resetsAt: now + 2 * 3600 + 14 * 60 },
      weekly: { usedPercent: 71, remainingPercent: 29, resetsAt: now + 3 * 86400 },
      planType: "Plus",
    },
  };
}

export function mockSessions(): SessionState[] {
  const now = Date.now();
  return [
    {
      sessionId: "sess-opus-1",
      project: "claude-monitor",
      model: "claude-opus-4-8",
      state: { kind: "working", tool: "Edit" },
      tokens: 1_240_000,
      updatedAt: now - 1_500,
    },
    {
      sessionId: "sess-sonnet-2",
      project: "CorePilot",
      model: "claude-sonnet-4-6",
      state: { kind: "thinking" },
      tokens: 412_000,
      updatedAt: now - 4_000,
    },
    {
      sessionId: "sess-haiku-3",
      project: "8111Reader",
      model: "claude-haiku-4-5",
      state: { kind: "waiting" },
      tokens: 88_000,
      updatedAt: now - 30_000,
    },
  ];
}

export function mockCurrent(): SessionState {
  return mockSessions()[0];
}

export function mockSettings(): AppSettings {
  return {
    monitorEnabled: true,
    notificationsEnabled: true,
    soundEnabled: true,
    soundVolume: 0.8,
    popoverOpacity: 85,
    currency: "USD",
    discordEnabled: false,
    discordClientId: null,
  };
}

/** Representative USD-based FX rates for standalone/mock rendering. */
export function mockFx(): FxPayload {
  return {
    base: "USD",
    rates: { USD: 1, CNY: 7.2, HKD: 7.8, EUR: 0.92, JPY: 150, GBP: 0.79 },
    fetchedAt: Math.floor(Date.now() / 1000),
    stale: false,
  };
}

// ---------------------------------------------------------------------------
// HTTP getters (fall back to mock on failure or in mock mode)
// ---------------------------------------------------------------------------

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(apiUrl(path), {
    headers: { accept: "application/json" },
  });
  if (!res.ok) throw new Error(`${path} -> HTTP ${res.status}`);
  return (await res.json()) as T;
}

export async function getSummary(range: RangeKey): Promise<Summary> {
  if (isMockForced()) return mockSummary(range);
  try {
    return await getJson<Summary>(`/api/summary?range=${range}`);
  } catch {
    return mockSummary(range);
  }
}

export async function getCurrent(): Promise<SessionState | null> {
  if (isMockForced()) return mockCurrent();
  try {
    return await getJson<SessionState | null>(`/api/current`);
  } catch {
    return mockCurrent();
  }
}

export async function getSessions(): Promise<SessionState[]> {
  if (isMockForced()) return mockSessions();
  try {
    return await getJson<SessionState[]>(`/api/sessions`);
  } catch {
    return mockSessions();
  }
}

export async function getLimits(): Promise<Limits> {
  if (isMockForced()) return mockLimits();
  try {
    return await getJson<Limits>(`/api/limits`);
  } catch {
    return mockLimits();
  }
}

export async function getSettings(): Promise<AppSettings> {
  if (isMockForced()) return mockSettings();
  try {
    return await getJson<AppSettings>(`/api/settings`);
  } catch {
    return mockSettings();
  }
}

/** Fetch the USD-based billing-currency FX rates (cached server-side, daily). */
export async function getFx(): Promise<FxPayload> {
  if (isMockForced()) return mockFx();
  try {
    return await getJson<FxPayload>(`/api/fx`);
  } catch {
    return mockFx();
  }
}

/**
 * Apply a partial settings patch via `PUT /api/settings`, returning the full
 * updated settings. In mock mode (or on failure) the patch is merged into the
 * mock baseline so the panel still reflects the change.
 */
export async function updateSettings(patch: AppSettingsPatch): Promise<AppSettings> {
  if (isMockForced()) return { ...mockSettings(), ...patch };
  try {
    const res = await fetch(apiUrl(`/api/settings`), {
      method: "PUT",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(patch),
    });
    if (!res.ok) throw new Error(`/api/settings -> HTTP ${res.status}`);
    return (await res.json()) as AppSettings;
  } catch {
    return { ...mockSettings(), ...patch };
  }
}

// ---------------------------------------------------------------------------
// SSE subscription (with a mock driver when no backend is present)
// ---------------------------------------------------------------------------

export type Unsubscribe = () => void;

const SSE_EVENTS = ["usage", "sessions", "import"] as const;

/**
 * Subscribe to the backend event stream. Calls onEvent for each typed event.
 * Returns an unsubscribe function. Uses exponential reconnect for backend
 * disconnects; mock mode is only used for standalone rendering or missing
 * EventSource support.
 */
export function subscribe(onEvent: (ev: CmServerEvent) => void): Unsubscribe {
  if (isMockForced() || typeof EventSource === "undefined") {
    return mockEventDriver(onEvent);
  }

  let es: EventSource | null = null;
  let closed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let reconnectDelay = 1_000;
  const maxReconnectDelay = 30_000;

  const clearReconnect = () => {
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  };

  const scheduleReconnect = () => {
    if (closed || reconnectTimer !== null) return;
    es?.close();
    es = null;
    const delay = reconnectDelay;
    reconnectDelay = Math.min(reconnectDelay * 2, maxReconnectDelay);
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, delay);
  };

  const connect = () => {
    if (closed) return;
    try {
      es = new EventSource(apiUrl("/events"));
    } catch {
      scheduleReconnect();
      return;
    }
    es.onopen = () => {
      reconnectDelay = 1_000;
    };
    es.onerror = () => {
      scheduleReconnect();
    };
    for (const name of SSE_EVENTS) {
      es.addEventListener(name, (e) => {
        try {
          const data = JSON.parse((e as MessageEvent).data);
          onEvent({ type: name, data } as CmServerEvent);
        } catch {
          /* ignore malformed frame */
        }
      });
    }
  };

  connect();

  return () => {
    closed = true;
    clearReconnect();
    es?.close();
  };
}

/** Drives synthetic SSE events on a timer for standalone/mock rendering. */
function mockEventDriver(onEvent: (ev: CmServerEvent) => void): Unsubscribe {
  const timers: ReturnType<typeof setInterval>[] = [];
  const states: SessionState["state"][] = [
    { kind: "thinking" },
    { kind: "working", tool: "Bash" },
    { kind: "working", tool: "Read" },
    { kind: "responding" },
    { kind: "waiting" },
    { kind: "idle" },
  ];
  let i = 0;
  const sessions = mockSessions();

  // Initial burst (next microtask, so subscribers are wired up first).
  const kick = setTimeout(() => {
    onEvent({ type: "sessions", data: sessions });
    onEvent({ type: "usage", data: { current: sessions[0] } });
    onEvent({ type: "import", data: { done: 551, total: 551 } });
  }, 0);

  // Cycle the lead session's state + tokens to show liveness.
  timers.push(
    setInterval(() => {
      i = (i + 1) % states.length;
      const lead = { ...sessions[0], state: states[i] };
      lead.tokens += Math.round(5_000 + Math.random() * 40_000);
      lead.updatedAt = Date.now();
      const next = [lead, ...sessions.slice(1)];
      onEvent({ type: "sessions", data: next });
      onEvent({ type: "usage", data: { current: lead } });
    }, 2_200),
  );

  return () => {
    clearTimeout(kick);
    for (const t of timers) clearInterval(t);
  };
}
