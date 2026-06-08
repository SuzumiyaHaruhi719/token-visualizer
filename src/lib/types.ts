// Shared DTO types — TS mirror of the Rust core DTOs.
// Server returns camelCase JSON (serde rename_all = "camelCase").

export interface Totals {
  tokens: number;
  input: number;
  output: number;
  cacheCreate: number;
  cacheRead: number;
  costUsd: number | null;
  cacheHitRate: number;
  messages: number;
  sessions: number;
}

export interface ByModel {
  model: string;
  tokens: number;
  costUsd: number | null;
}

export interface ByProject {
  project: string;
  tokens: number;
}

export interface TimeseriesBucket {
  bucket: string;
  input: number;
  output: number;
  cacheCreate: number;
  cacheRead: number;
}

export type Source = "claude" | "codex";

export interface BySource {
  source: Source;
  tokens: number;
  costUsd: number | null;
}

export interface Summary {
  range: string;
  totals: Totals;
  byModel: ByModel[];
  byProject: ByProject[];
  bySource: BySource[];
  timeseries: TimeseriesBucket[];
}

// /api/limits ---------------------------------------------------------------

/** A rate-limit window. `resetsAt` is epoch SECONDS. */
export interface RateWindow {
  usedPercent: number;
  remainingPercent: number;
  resetsAt: number;
}

export interface ClaudeSessionLimit {
  project: string;
  model: string;
  tokens: number;
  state: PetState;
}

export interface ClaudeLimits {
  session: ClaudeSessionLimit | null;
  fiveHour: RateWindow | null;
  weekly: RateWindow | null;
  note: string;
}

export interface CodexSessionLimit {
  model: string;
  tokens: number;
}

export interface CodexLimits {
  session: CodexSessionLimit | null;
  fiveHour: RateWindow | null;
  weekly: RateWindow | null;
  planType: string | null;
}

export interface Limits {
  claude: ClaudeLimits;
  codex: CodexLimits;
}

export type PetStateKind =
  | "idle"
  | "thinking"
  | "responding"
  | "waiting"
  | "sleeping"
  | "working";

export type PetState =
  | { kind: "idle" | "thinking" | "responding" | "waiting" | "sleeping" }
  | { kind: "working"; tool: string | null };

export interface SessionState {
  sessionId: string;
  project: string;
  model: string;
  state: PetState;
  tokens: number;
  updatedAt: number;
}

export type RangeKey = "today" | "7d" | "30d" | "all";

// SSE event payloads
export interface UsageEvent {
  current: SessionState | null;
}
export type SessionsEvent = SessionState[];
export interface ImportEvent {
  done: number;
  total: number;
}

export type CmServerEvent =
  | { type: "usage"; data: UsageEvent }
  | { type: "sessions"; data: SessionsEvent }
  | { type: "import"; data: ImportEvent };
