//! Core data model: usage, parsed events, line kinds, and the pet/session DTOs.
//!
//! The DTO structs (`Summary`, `Totals`, `SessionState`, breakdown rows) carry
//! `#[serde(rename_all = "camelCase")]` because they are serialized to JSON and
//! consumed verbatim by the TypeScript frontend. `PetState` is serde-tagged so
//! it round-trips as `{ "kind": "working", "tool": "Bash" }` etc.

use serde::{Deserialize, Serialize};

/// Which tool a usage event originated from. Serialized lowercase
/// (`"claude"` / `"codex"`) for the frontend and the `events.source` column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Claude Code (`~/.claude/projects/**/*.jsonl`).
    #[default]
    Claude,
    /// OpenAI Codex CLI (`~/.codex/sessions/**/rollout-*.jsonl`).
    Codex,
}

impl Source {
    /// The lowercase string used in the `events.source` column and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
        }
    }

    /// Parse from the stored column string; unknown values map to `Claude`.
    pub fn from_str_or_claude(s: &str) -> Self {
        match s {
            "codex" => Source::Codex,
            _ => Source::Claude,
        }
    }
}

/// Token usage extracted from one assistant `message.usage` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Non-cached input tokens.
    pub input: i64,
    /// Output tokens.
    pub output: i64,
    /// Cache write tokens (first time content is cached; billed at 1.25x input).
    pub cache_create: i64,
    /// Cache read tokens (cache hit; billed at 0.10x input).
    pub cache_read: i64,
    /// Server-side `web_search` tool invocations.
    pub web_search: i64,
    /// Server-side `web_fetch` tool invocations.
    pub web_fetch: i64,
}

impl Usage {
    /// Total token count across the four token-bearing fields.
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}

/// A single assistant message that carried token usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedEvent {
    /// Dedup key (top-level `requestId`, fallback to `uuid`).
    pub request_id: String,
    /// Epoch milliseconds, UTC.
    pub ts: i64,
    pub session_id: String,
    /// Friendly project name derived from the `cwd` basename.
    pub project: String,
    pub model: String,
    pub usage: Usage,
    /// Which tool produced this event. Defaults to [`Source::Claude`] so older
    /// callers and deserialized rows without the field keep working.
    #[serde(default)]
    pub source: Source,
}

/// What a single jsonl line represents (only the variants we care about).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// Assistant message that carried `message.usage`.
    Assistant(ParsedEvent),
    /// Assistant `thinking` content block.
    Thinking,
    /// A tool call was started.
    ToolUse { id: String, name: String },
    /// A tool finished (`tool_result` block).
    ToolResult { tool_use_id: String },
    /// `stop_reason == "end_turn"` with no tool_use.
    EndTurn,
    /// Anything else (user/system/summary/sidechain) — ignored for usage.
    Other,
}

/// The desktop-pet animation state for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "tool", rename_all = "snake_case")]
pub enum PetState {
    Idle,
    Thinking,
    /// Running a tool; carries the tool name when known.
    Working(Option<String>),
    Responding,
    Waiting,
    Sleeping,
}

/// Live state of one active session — drives both the dashboard "current"
/// strip and the per-session pet windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub state: PetState,
    /// Running token total for this session.
    pub tokens: i64,
    pub updated_at: i64,
}

/// Aggregate totals across a time range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub tokens: i64,
    pub input: i64,
    pub output: i64,
    pub cache_create: i64,
    pub cache_read: i64,
    /// `None` when any model in the range had no known price.
    pub cost_usd: Option<f64>,
    /// `cache_read / (input + cache_create + cache_read)`.
    pub cache_hit_rate: f64,
    pub messages: i64,
    pub sessions: i64,
}

/// Per-model breakdown row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBreakdown {
    pub model: String,
    pub tokens: i64,
    pub cost_usd: Option<f64>,
}

/// Per-project breakdown row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBreakdown {
    pub project: String,
    pub tokens: i64,
}

/// Per-source breakdown row (Claude vs Codex).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakdown {
    pub source: Source,
    pub tokens: i64,
    pub cost_usd: Option<f64>,
}

/// One time-bucket (daily) of the stacked token timeseries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeseriesBucket {
    /// Bucket label, e.g. `2026-06-05`.
    pub bucket: String,
    pub input: i64,
    pub output: i64,
    pub cache_create: i64,
    pub cache_read: i64,
}

/// Top-level dashboard summary DTO for a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub range: String,
    pub totals: Totals,
    pub by_model: Vec<ModelBreakdown>,
    pub by_project: Vec<ProjectBreakdown>,
    /// Per-source (Claude vs Codex) token + cost breakdown.
    pub by_source: Vec<SourceBreakdown>,
    pub timeseries: Vec<TimeseriesBucket>,
}
