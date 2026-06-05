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

export interface Summary {
  range: string;
  totals: Totals;
  byModel: ByModel[];
  byProject: ByProject[];
  timeseries: TimeseriesBucket[];
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
