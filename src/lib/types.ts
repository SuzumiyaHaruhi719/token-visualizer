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

export type Source = "claude" | "codex" | "deepseek";

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
  /** Epoch millis of this session's LAST ACTIVITY (newest log line), NOT the
   *  poll time. Render staleness from it (e.g. "1m 49s ago"); a live session
   *  mid-work keeps its real state while this value ages. */
  updatedAt: number;
  /** The most recent USER prompt text for this session (a real prompt, not a
   *  tool result or injected wrapper), single-line and capped. Shown in place of
   *  the project name. Optional so existing fixtures / payloads without the field
   *  keep working; absent is treated as empty. */
  lastUserMessage?: string;
  /** Which agent the live session belongs to. Optional so existing fixtures /
   *  payloads without the field keep working; absent is treated as "claude". */
  source?: Source;
}

export type RangeKey = "today" | "month" | "7d" | "30d" | "all";

// /api/settings -------------------------------------------------------------

/** Runtime + persisted app settings (mirrors the server `/api/settings` shape). */
export interface AppSettings {
  monitorEnabled: boolean;
  /** Whether the session-end notification (toast + taskbar flash) fires. */
  notificationsEnabled: boolean;
  soundEnabled: boolean;
  /** Chime volume as a 0..1 float. */
  soundVolume: number;
  /** Popover background opacity percent (0..100) — alpha of the acrylic CSS tint. */
  popoverOpacity: number;
  /** Billing display currency ISO code (USD/CNY/HKD/EUR/JPY/GBP). */
  currency: string;
  discordEnabled: boolean;
  discordClientId: string | null;
}

/** Partial settings update: any subset of {@link AppSettings} fields. */
export type AppSettingsPatch = Partial<AppSettings>;

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
