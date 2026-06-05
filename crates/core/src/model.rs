//! Core data model: usage, parsed events, line kinds, and the pet/session DTOs.
//!
//! The DTO structs (`Summary`, `Totals`, `SessionState`, breakdown rows) carry
//! `#[serde(rename_all = "camelCase")]` because they are serialized to JSON and
//! consumed verbatim by the TypeScript frontend. `PetState` is serde-tagged so
//! it round-trips as `{ "kind": "working", "tool": "Bash" }` etc.

use serde::{Deserialize, Serialize};

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
    pub timeseries: Vec<TimeseriesBucket>,
}
