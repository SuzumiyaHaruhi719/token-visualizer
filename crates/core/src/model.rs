//! Core data model: usage, parsed events, line kinds, and the pet/session DTOs.
//!
//! The DTO structs (`Summary`, `Totals`, `SessionState`, breakdown rows) carry
//! `#[serde(rename_all = "camelCase")]` because they are serialized to JSON and
//! consumed verbatim by the TypeScript frontend. `PetState` is serde-tagged so
//! it round-trips as `{ "kind": "working", "tool": "Bash" }` etc.

use serde::{Deserialize, Serialize};

/// Which tool a usage event originated from. Serialized lowercase
/// (`"claude"` / `"codex"` / `"deepseek"`) for the frontend and the
/// `events.source` column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Claude Code (`~/.claude/projects/**/*.jsonl`).
    #[default]
    Claude,
    /// OpenAI Codex CLI (`~/.codex/sessions/**/rollout-*.jsonl`).
    Codex,
    /// Reasonix (a DeepSeek desktop client, `~/.reasonix/usage.jsonl` +
    /// `~/.reasonix/sessions/`). Serialized `"deepseek"` so the by-source UI
    /// labels it by the model provider the user sees.
    DeepSeek,
}

impl Source {
    /// The lowercase string used in the `events.source` column and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::DeepSeek => "deepseek",
        }
    }

    /// Parse from the stored column string; unknown values map to `Claude`.
    pub fn from_str_or_claude(s: &str) -> Self {
        match s {
            "codex" => Source::Codex,
            "deepseek" => Source::DeepSeek,
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

/// The dominant content of an assistant message that carried `message.usage`.
///
/// Real Claude Code jsonl attaches `message.usage` to EVERY assistant message —
/// including pure-reasoning and tool-call turns — so the usage block alone cannot
/// tell us what the model was actually doing. This captures the content block
/// that drives the live state: reasoning, a tool call, or visible text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantContent {
    /// The message body is a `thinking` block (extended reasoning) only.
    Thinking,
    /// The message issued a `tool_use` call (the model is about to run a tool).
    Tool { id: String, name: String },
    /// The message emitted visible `text` (or anything else) — responding.
    Text,
}

/// What a single jsonl line represents (only the variants we care about).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// Assistant message that carried `message.usage`. `content` records what the
    /// model was doing (reasoning / tool call / text) for live-state derivation;
    /// the [`ParsedEvent`] carries the token usage for billing.
    Assistant {
        event: ParsedEvent,
        content: AssistantContent,
    },
    /// Assistant `thinking` content block with no usage (streaming reasoning).
    Thinking,
    /// A tool call was started (a `tool_use` block with no usage).
    ToolUse { id: String, name: String },
    /// A tool finished (`tool_result` block).
    ToolResult { tool_use_id: String },
    /// `stop_reason == "end_turn"` with no tool_use.
    EndTurn,
    /// A real USER prompt line (`message.role == "user"` with text content that
    /// is NOT a tool result and NOT an injected wrapper). Carries the cleaned,
    /// capped prompt text so the live-state layer can surface the last one. Not
    /// billable; ignored by usage aggregation like every non-assistant kind.
    UserText(String),
    /// Anything else (system/summary/sidechain/tool-result-only user) — ignored.
    Other,
}

impl LineKind {
    /// The billable [`ParsedEvent`] this line carries, if any. Only usage-bearing
    /// assistant lines produce an event; all other kinds return `None`.
    pub fn event(&self) -> Option<&ParsedEvent> {
        match self {
            LineKind::Assistant { event, .. } => Some(event),
            _ => None,
        }
    }
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

/// Live state of one active session — drives the dashboard "current" session
/// strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub state: PetState,
    /// Running token total for this session.
    pub tokens: i64,
    /// Epoch millis of this session's LAST ACTIVITY (newest jsonl/rollout line
    /// timestamp), NOT the poll time. The frontend renders staleness ("Nm ago")
    /// from it, so a live-but-quiet session (mid-reasoning, not writing every
    /// second) keeps its real state while this value ages — that freshness is
    /// what replaces the old snap-to-`Idle` behavior.
    pub updated_at: i64,
    /// The most recent USER message text for this session (a real prompt, not a
    /// tool result or injected wrapper), trimmed to a single line and capped.
    /// Empty when none was found in the scanned tail. The frontend shows this in
    /// place of the project name. `#[serde(default)]` so older payloads / test
    /// fixtures without the field still deserialize.
    #[serde(default)]
    pub last_user_message: String,
    /// Which agent this live session belongs to (Claude Code vs Codex CLI vs
    /// Reasonix/DeepSeek), so the UI can tell them apart. Defaults to
    /// [`Source::Claude`] so existing callers / deserialized payloads without
    /// the field keep working.
    #[serde(default)]
    pub source: Source,
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

/// Per-source breakdown row (Claude vs Codex vs DeepSeek).
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
    /// Per-source (Claude vs Codex vs DeepSeek) token + cost breakdown.
    pub by_source: Vec<SourceBreakdown>,
    pub timeseries: Vec<TimeseriesBucket>,
}
