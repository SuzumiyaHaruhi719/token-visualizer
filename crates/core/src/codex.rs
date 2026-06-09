//! Tolerant parser for OpenAI Codex CLI rollout logs + mapping into our model.
//!
//! Codex writes one JSON object per line to
//! `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ISO>-<uuid>.jsonl`. Each line is
//! `{"timestamp","type","payload":{...}}`. We care about three payload shapes:
//!
//! * `type:"event_msg"` + `payload.type:"token_count"` — carries cumulative and
//!   per-turn token usage plus the account-level rate limits.
//! * `turn_context` / `session_meta` — carry the active `model` id.
//! * `session_meta` — carries the session `id`.
//!
//! Like [`crate::parser`], every failure collapses to [`CodexLine::Other`]; this
//! module NEVER panics on partial, garbage, or unexpected input. The Codex logs
//! are only ever READ.

use std::path::{Path, PathBuf};

use serde_json::Value;
use walkdir::WalkDir;

use crate::model::{ParsedEvent, Source, Usage};

/// Codex cumulative or per-turn token usage (the `total_token_usage` /
/// `last_token_usage` shape).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexUsage {
    pub input: i64,
    pub cached_input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub total: i64,
}

/// One rate-limit window (`primary` = 5h, `secondary` = weekly).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateWindow {
    pub used_percent: f64,
    pub window_minutes: i64,
    pub resets_at: i64,
}

/// The account-level rate limits attached to a `token_count` event.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimits {
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
    pub plan_type: Option<String>,
}

/// What a single Codex rollout line represents (only the variants we care about).
#[derive(Debug, Clone, PartialEq)]
pub enum CodexLine {
    /// A `token_count` event: cumulative + per-turn usage and rate limits.
    TokenCount {
        total: CodexUsage,
        last: CodexUsage,
        rate_limits: Option<RateLimits>,
    },
    /// The active model id (from `turn_context` / `session_meta`).
    Model(String),
    /// Session metadata carrying the session `id`.
    SessionMeta { id: String },
    /// Anything else — ignored.
    Other,
}

/// Parse a single Codex rollout line. Tolerant: any failure yields
/// [`CodexLine::Other`].
pub fn parse_codex_line(line: &str) -> CodexLine {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return CodexLine::Other,
    };
    classify(&v)
}

fn classify(v: &Value) -> CodexLine {
    let line_type = v.get("type").and_then(Value::as_str).unwrap_or_default();
    let payload = v.get("payload");

    // 1. token_count event (the only thing we bill on).
    if line_type == "event_msg" {
        let is_token_count =
            payload.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("token_count");
        if is_token_count {
            if let Some(p) = payload {
                return parse_token_count(p);
            }
        }
    }

    // 2. session_meta — carries the session id (and possibly a model).
    if line_type == "session_meta" {
        if let Some(id) = payload
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return CodexLine::SessionMeta { id: id.to_string() };
        }
    }

    // 3. Any payload carrying a model id (turn_context, session_meta, etc).
    if let Some(model) = payload
        .and_then(|p| p.get("model"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return CodexLine::Model(model.to_string());
    }

    CodexLine::Other
}

/// The live activity a Codex rollout line represents, mirroring the Claude
/// [`crate::model::AssistantContent`] so a Codex session can be classified into
/// the same pet states. Codex streams each step as its own line, so unlike
/// Claude there is no usage block to look past — the `payload.type` IS the
/// activity:
///
/// * `response_item` / `reasoning` — the model is reasoning -> Thinking,
/// * `response_item` / `function_call` — a tool call (shell etc.) -> Working,
/// * `response_item` / `function_call_output` — a tool finished,
/// * `event_msg` / `agent_message` or an assistant `message` — visible text
///   -> Responding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexActivity {
    /// A `reasoning` item: the model is thinking (no visible output yet).
    Reasoning,
    /// A `function_call` (tool invocation), carrying the tool name + call id.
    ToolCall { call_id: String, name: String },
    /// A `function_call_output` (tool result) for the given call id.
    ToolOutput { call_id: String },
    /// The model emitted a visible reply (`agent_message` / assistant `message`).
    Message,
    /// A real USER prompt (`role:"user"` `input_text`, injected wrappers
    /// excluded). The user just submitted and the model has not produced any
    /// output yet — an ACTIVE turn (about to think), so a trailing one maps to
    /// Thinking, never a stale Idle. Mirrors Claude's [`crate::model::LineKind`]
    /// `UserText -> Thinking`.
    UserPrompt,
    /// Anything else — carries no activity signal.
    Other,
}

/// Extract a real USER prompt from a Codex rollout line, or `None`.
///
/// A Codex user prompt is a `response_item` / `message` line with `role:"user"`
/// whose content is one or more `input_text` blocks. Codex injects synthetic
/// user messages too (the `<environment_context>` / `<user_instructions>`
/// preamble), so injected wrappers are filtered out via
/// [`crate::text::is_injected_user_text`]. The result is cleaned + capped to a
/// single line. Tolerant: any parse failure / non-matching shape yields `None`.
pub fn codex_user_text(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    user_text_from_value(&v)
}

/// [`codex_user_text`] over an already-parsed value (shared with
/// [`classify_activity`] so a user line is parsed once per tick, not twice).
fn user_text_from_value(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str)? != "response_item" {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type").and_then(Value::as_str)? != "message" {
        return None;
    }
    if payload.get("role").and_then(Value::as_str)? != "user" {
        return None;
    }
    let content = payload.get("content").and_then(Value::as_array)?;
    // Concatenate the input_text blocks (a prompt may be split across several).
    let mut raw = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("input_text") {
            if let Some(t) = block.get("text").and_then(Value::as_str) {
                if !raw.is_empty() {
                    raw.push(' ');
                }
                raw.push_str(t);
            }
        }
    }
    if raw.is_empty() || crate::text::is_injected_user_text(&raw) {
        return None;
    }
    let cleaned = crate::text::clean_user_text(&raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Classify a Codex rollout line into a live [`CodexActivity`]. Tolerant: any
/// parse failure or unrecognized shape yields [`CodexActivity::Other`].
///
/// This is the Codex counterpart of [`crate::parser::parse_line`]'s content
/// classification: reasoning items count as an ACTIVE thinking signal so a Codex
/// session that is reasoning (but not yet emitting text or tool calls) is shown
/// as Thinking, never Idle.
pub fn codex_activity(line: &str) -> CodexActivity {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return CodexActivity::Other,
    };
    classify_activity(&v)
}

fn classify_activity(v: &Value) -> CodexActivity {
    let line_type = v.get("type").and_then(Value::as_str).unwrap_or_default();
    let payload = match v.get("payload") {
        Some(p) => p,
        None => return CodexActivity::Other,
    };
    let payload_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match (line_type, payload_type) {
        ("response_item", "reasoning") => CodexActivity::Reasoning,
        ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
            CodexActivity::ToolCall {
                call_id: str_field(payload, "call_id").unwrap_or_default(),
                name: str_field(payload, "name").unwrap_or_default(),
            }
        }
        ("response_item", "function_call_output")
        | ("response_item", "custom_tool_call_output") => CodexActivity::ToolOutput {
            call_id: str_field(payload, "call_id").unwrap_or_default(),
        },
        // The model's visible reply: an `agent_message` event or an assistant
        // `message` response item (developer/user messages are NOT the model).
        ("event_msg", "agent_message") => CodexActivity::Message,
        ("response_item", "message")
            if payload.get("role").and_then(Value::as_str) == Some("assistant") =>
        {
            CodexActivity::Message
        }
        // A real user prompt is an active turn (the model is about to work). A
        // synthetic injected message (`<environment_context>` etc.) is NOT — it
        // carries no real prompt, so `codex_user_text` returns `None` for it.
        ("response_item", "message")
            if payload.get("role").and_then(Value::as_str) == Some("user")
                && user_text_from_value(v).is_some() =>
        {
            CodexActivity::UserPrompt
        }
        _ => CodexActivity::Other,
    }
}

/// Derive a live [`crate::model::PetState`] from a window of Codex activities
/// (oldest-first, the tail of a rollout). This is the Codex counterpart of
/// [`crate::state::derive_from_lines`] and applies the SAME tool-pairing rule:
///
/// * the LAST `function_call` with no later `function_call_output` for the same
///   `call_id` means a tool is still running -> [`PetState::Working`] (tool name),
/// * otherwise the last meaningful activity decides:
///   * `reasoning` -> [`PetState::Thinking`] (the model is reasoning),
///   * a trailing `function_call_output` (the tool finished; the model is now
///     reasoning about the result, with no end-turn marker in Codex) ->
///     [`PetState::Thinking`],
///   * a visible `agent_message` / assistant `message` -> [`PetState::Responding`],
/// * nothing meaningful -> [`PetState::Idle`].
///
/// Pure + total: an empty window yields [`PetState::Idle`]; it never panics.
pub fn derive_codex_state(activities: &[CodexActivity]) -> crate::model::PetState {
    use crate::model::PetState;

    if let Some(name) = last_unmatched_codex_tool(activities) {
        let tool = if name.is_empty() { None } else { Some(name) };
        return PetState::Working(tool);
    }

    for activity in activities.iter().rev() {
        match activity {
            CodexActivity::Reasoning => return PetState::Thinking,
            CodexActivity::ToolOutput { .. } => return PetState::Thinking,
            CodexActivity::Message => return PetState::Responding,
            // The user just submitted and the model has not produced output yet:
            // an active turn (about to think), never a stale Idle.
            CodexActivity::UserPrompt => return PetState::Thinking,
            // A matched ToolCall (the unmatched case is handled above) means the
            // model already moved past it — keep scanning for the real tail state.
            CodexActivity::ToolCall { .. } => continue,
            CodexActivity::Other => continue,
        }
    }
    PetState::Idle
}

/// The tool name of the last `function_call` that has no matching
/// `function_call_output` (same `call_id`) appearing AFTER it in the window —
/// i.e. a tool that is still running. Mirrors Claude's `last_unmatched_tool`.
fn last_unmatched_codex_tool(activities: &[CodexActivity]) -> Option<String> {
    for (i, activity) in activities.iter().enumerate().rev() {
        let CodexActivity::ToolCall { call_id, name } = activity else {
            continue;
        };
        // An empty call_id can never be paired (a real Codex call always carries
        // one); treating "" == "" as a match would wrongly cancel an in-flight
        // tool. So a call with an empty id is always considered unmatched.
        let matched_after = !call_id.is_empty()
            && activities[i + 1..].iter().any(|a| {
                matches!(a, CodexActivity::ToolOutput { call_id: out_id } if out_id == call_id)
            });
        if !matched_after {
            return Some(name.clone());
        }
    }
    None
}

/// Read a string field, returning `None` when absent or non-string.
fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse a `token_count` payload. `info` may be `null` (early in a session); in
/// that case both usages default to zero but any rate limits are still captured.
fn parse_token_count(payload: &Value) -> CodexLine {
    let info = payload.get("info");
    let total = info
        .and_then(|i| i.get("total_token_usage"))
        .map(parse_codex_usage)
        .unwrap_or_default();
    let last = info
        .and_then(|i| i.get("last_token_usage"))
        .map(parse_codex_usage)
        .unwrap_or_default();
    let rate_limits = payload.get("rate_limits").and_then(parse_rate_limits);
    CodexLine::TokenCount {
        total,
        last,
        rate_limits,
    }
}

fn parse_codex_usage(v: &Value) -> CodexUsage {
    CodexUsage {
        input: int_field(v, "input_tokens"),
        cached_input: int_field(v, "cached_input_tokens"),
        output: int_field(v, "output_tokens"),
        reasoning: int_field(v, "reasoning_output_tokens"),
        total: int_field(v, "total_tokens"),
    }
}

fn parse_rate_limits(v: &Value) -> Option<RateLimits> {
    if !v.is_object() {
        return None;
    }
    let primary = v.get("primary").and_then(parse_rate_window);
    let secondary = v.get("secondary").and_then(parse_rate_window);
    let plan_type = v
        .get("plan_type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if primary.is_none() && secondary.is_none() && plan_type.is_none() {
        return None;
    }
    Some(RateLimits {
        primary,
        secondary,
        plan_type,
    })
}

fn parse_rate_window(v: &Value) -> Option<RateWindow> {
    if !v.is_object() {
        return None;
    }
    Some(RateWindow {
        used_percent: float_field(v, "used_percent"),
        window_minutes: int_field(v, "window_minutes"),
        resets_at: int_field(v, "resets_at"),
    })
}

/// Map a Codex per-turn usage into our generic [`Usage`].
///
/// Codex `input_tokens` already INCLUDES the cached tokens, so the non-cached
/// input is `input - cached`. Reasoning output tokens fold into `output`; Codex
/// has no separate cache-write counter so `cache_create` is always 0.
pub fn codex_usage_to_usage(u: &CodexUsage) -> Usage {
    Usage {
        input: (u.input - u.cached_input).max(0),
        output: u.output + u.reasoning,
        cache_create: 0,
        cache_read: u.cached_input,
        web_search: 0,
        web_fetch: 0,
    }
}

/// Build a [`ParsedEvent`] for a Codex `last_token_usage` turn.
///
/// The dedup key is `codex:<session_id>:<line_offset>` so the same turn read
/// twice (re-import / overlapping watch) collapses to one row.
pub fn codex_event(
    last: &CodexUsage,
    session_id: &str,
    model: &str,
    project: &str,
    ts: i64,
    line_offset: i64,
) -> ParsedEvent {
    ParsedEvent {
        request_id: format!("codex:{session_id}:{line_offset}"),
        ts,
        session_id: session_id.to_string(),
        project: project.to_string(),
        model: model.to_string(),
        usage: codex_usage_to_usage(last),
        source: Source::Codex,
    }
}

/// A snapshot of the latest Codex session's usage + rate limits, read live from
/// the most-recently-modified rollout file (drives `GET /api/limits`).
#[derive(Debug, Clone, PartialEq)]
pub struct CodexSnapshot {
    pub session_id: String,
    pub model: String,
    /// The latest cumulative usage for the session.
    pub total: CodexUsage,
    pub rate_limits: Option<RateLimits>,
}

/// Find the most-recently-modified `rollout-*.jsonl` under `codex_sessions_dir`.
pub fn latest_rollout(codex_sessions_dir: &Path) -> Option<PathBuf> {
    WalkDir::new(codex_sessions_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| {
            p.extension().map(|x| x == "jsonl").unwrap_or(false)
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("rollout-"))
                    .unwrap_or(false)
        })
        .max_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
        })
}

/// Read the latest session snapshot from the most-recently-modified rollout in
/// `codex_sessions_dir`. Returns `None` when there is no rollout (or it has no
/// `token_count` event yet). Reads the whole file; cheap enough per request.
pub fn latest_snapshot(codex_sessions_dir: &Path) -> Option<CodexSnapshot> {
    let path = latest_rollout(codex_sessions_dir)?;
    let text = std::fs::read_to_string(&path).ok()?;

    let session_id = session_id_from_path(&path);
    let mut model = String::new();
    let mut session_id = session_id;
    let mut latest: Option<(CodexUsage, Option<RateLimits>)> = None;

    for line in text.lines() {
        match parse_codex_line(line) {
            CodexLine::Model(m) => model = m,
            CodexLine::SessionMeta { id } => session_id = id,
            CodexLine::TokenCount {
                total,
                rate_limits,
                ..
            } => {
                latest = Some((total, rate_limits));
            }
            CodexLine::Other => {}
        }
    }

    latest.map(|(total, rate_limits)| CodexSnapshot {
        session_id,
        model,
        total,
        rate_limits,
    })
}

/// Session uuid from a `rollout-<ISO>-<uuid>.jsonl` path (trailing five groups).
fn session_id_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() >= 5 {
        parts[parts.len() - 5..].join("-")
    } else {
        stem.to_string()
    }
}

fn int_field(v: &Value, key: &str) -> i64 {
    match v.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

fn float_field(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real-shape token_count line captured from this machine.
    const TOKEN_COUNT_LINE: &str = r#"{"timestamp":"2026-03-21T05:12:04.528Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":184499,"cached_input_tokens":142336,"output_tokens":2547,"reasoning_output_tokens":1074,"total_tokens":187046},"last_token_usage":{"input_tokens":27477,"cached_input_tokens":27136,"output_tokens":124,"reasoning_output_tokens":21,"total_tokens":27601},"model_context_window":258400},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1774087355},"secondary":{"used_percent":0.0,"window_minutes":10080,"resets_at":1774674155},"credits":null,"plan_type":"plus"}}}"#;

    const SESSION_META_LINE: &str = r#"{"timestamp":"2026-03-21T05:04:40.977Z","type":"session_meta","payload":{"id":"019d0ec7-c4ce-71e0-b486-88e6f33baa31","cwd":"C:\\Users\\Thomas\\Documents\\New project"}}"#;

    const TURN_CONTEXT_LINE: &str = r#"{"timestamp":"2026-03-21T05:04:40.979Z","type":"turn_context","payload":{"turn_id":"x","cwd":"C:\\x","model":"gpt-5.4"}}"#;

    #[test]
    fn parses_token_count_usage_and_limits() {
        match parse_codex_line(TOKEN_COUNT_LINE) {
            CodexLine::TokenCount {
                total,
                last,
                rate_limits,
            } => {
                assert_eq!(total.input, 184_499);
                assert_eq!(total.cached_input, 142_336);
                assert_eq!(total.total, 187_046);
                assert_eq!(last.input, 27_477);
                assert_eq!(last.reasoning, 21);

                let rl = rate_limits.expect("rate limits present");
                let primary = rl.primary.expect("primary window");
                assert_eq!(primary.window_minutes, 300);
                assert_eq!(primary.resets_at, 1_774_087_355);
                assert!((primary.used_percent - 1.0).abs() < f64::EPSILON);
                let secondary = rl.secondary.expect("secondary window");
                assert_eq!(secondary.window_minutes, 10_080);
                assert_eq!(rl.plan_type.as_deref(), Some("plus"));
            }
            other => panic!("expected TokenCount, got {other:?}"),
        }
    }

    #[test]
    fn maps_last_usage_into_usage() {
        let last = CodexUsage {
            input: 27_477,
            cached_input: 27_136,
            output: 124,
            reasoning: 21,
            total: 27_601,
        };
        let usage = codex_usage_to_usage(&last);
        assert_eq!(usage.input, 27_477 - 27_136); // 341
        assert_eq!(usage.cache_read, 27_136);
        assert_eq!(usage.output, 124 + 21); // 145
        assert_eq!(usage.cache_create, 0);
    }

    #[test]
    fn input_minus_cached_never_negative() {
        let last = CodexUsage {
            input: 10,
            cached_input: 50,
            ..Default::default()
        };
        assert_eq!(codex_usage_to_usage(&last).input, 0);
    }

    #[test]
    fn parses_session_meta_id() {
        assert_eq!(
            parse_codex_line(SESSION_META_LINE),
            CodexLine::SessionMeta {
                id: "019d0ec7-c4ce-71e0-b486-88e6f33baa31".to_string()
            }
        );
    }

    #[test]
    fn parses_model_from_turn_context() {
        assert_eq!(
            parse_codex_line(TURN_CONTEXT_LINE),
            CodexLine::Model("gpt-5.4".to_string())
        );
    }

    #[test]
    fn null_info_yields_zero_usage_but_keeps_limits() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":0.0,"window_minutes":300,"resets_at":1},"secondary":{"used_percent":0.0,"window_minutes":10080,"resets_at":2},"plan_type":"prolite"}}}"#;
        match parse_codex_line(line) {
            CodexLine::TokenCount {
                total,
                last,
                rate_limits,
            } => {
                assert_eq!(total, CodexUsage::default());
                assert_eq!(last, CodexUsage::default());
                assert!(rate_limits.is_some());
            }
            other => panic!("expected TokenCount, got {other:?}"),
        }
    }

    #[test]
    fn garbage_never_panics() {
        assert_eq!(parse_codex_line(""), CodexLine::Other);
        assert_eq!(parse_codex_line("{not json"), CodexLine::Other);
        assert_eq!(parse_codex_line("{}"), CodexLine::Other);
        assert_eq!(parse_codex_line(r#"{"type":"x","payload":{}}"#), CodexLine::Other);
    }

    #[test]
    fn builds_dedup_request_id() {
        let last = CodexUsage::default();
        let e = codex_event(&last, "sess-9", "gpt-5.4", "proj", 123, 4096);
        assert_eq!(e.request_id, "codex:sess-9:4096");
        assert_eq!(e.source, Source::Codex);
        assert_eq!(e.model, "gpt-5.4");
    }

    // --- Codex activity classification (real captured shapes) --------------

    // A reasoning item: the model is thinking. This is the Codex equivalent of a
    // Claude thinking block and MUST count as an active thinking signal.
    const REASONING_LINE: &str = r#"{"timestamp":"2026-06-08T10:07:33.683Z","type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"gAAAA..."}}"#;

    const FUNCTION_CALL_LINE: &str = r#"{"timestamp":"2026-06-08T10:07:36.097Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"ls\"}","call_id":"call_abc"}}"#;

    const FUNCTION_OUTPUT_LINE: &str = r#"{"timestamp":"2026-06-08T10:07:41.599Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_abc","output":"Exit code: 0"}}"#;

    const AGENT_MESSAGE_LINE: &str = r#"{"timestamp":"2026-06-08T10:07:34.492Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll use the skill.","phase":"commentary"}}"#;

    #[test]
    fn reasoning_item_is_thinking_activity() {
        assert_eq!(codex_activity(REASONING_LINE), CodexActivity::Reasoning);
    }

    #[test]
    fn function_call_is_tool_activity_with_name() {
        assert_eq!(
            codex_activity(FUNCTION_CALL_LINE),
            CodexActivity::ToolCall {
                call_id: "call_abc".into(),
                name: "shell_command".into(),
            }
        );
    }

    #[test]
    fn function_call_output_is_tool_output() {
        assert_eq!(
            codex_activity(FUNCTION_OUTPUT_LINE),
            CodexActivity::ToolOutput {
                call_id: "call_abc".into(),
            }
        );
    }

    #[test]
    fn agent_message_is_responding_activity() {
        assert_eq!(codex_activity(AGENT_MESSAGE_LINE), CodexActivity::Message);
    }

    #[test]
    fn real_user_message_is_user_prompt_activity() {
        assert_eq!(codex_activity(USER_PROMPT_LINE), CodexActivity::UserPrompt);
    }

    #[test]
    fn injected_user_message_is_not_an_activity() {
        // A synthetic <environment_context> user message carries no real prompt,
        // so it is NOT an active turn.
        assert_eq!(codex_activity(ENV_CONTEXT_LINE), CodexActivity::Other);
    }

    // --- Codex user-prompt extraction (real captured shapes) ---------------

    const USER_PROMPT_LINE: &str = r#"{"timestamp":"2026-06-09T06:45:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Audit an UNCOMMITTED change in CorePilot"}]}}"#;

    const ENV_CONTEXT_LINE: &str = r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>C:\\x</cwd>\n</environment_context>"}]}}"#;

    const DEVELOPER_LINE: &str = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions> ..."}]}}"#;

    #[test]
    fn extracts_real_user_prompt() {
        assert_eq!(
            codex_user_text(USER_PROMPT_LINE).as_deref(),
            Some("Audit an UNCOMMITTED change in CorePilot")
        );
    }

    #[test]
    fn skips_injected_environment_context() {
        assert_eq!(codex_user_text(ENV_CONTEXT_LINE), None);
    }

    #[test]
    fn skips_developer_role() {
        assert_eq!(codex_user_text(DEVELOPER_LINE), None);
    }

    #[test]
    fn assistant_message_is_not_a_user_prompt() {
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}}"#;
        assert_eq!(codex_user_text(line), None);
    }

    #[test]
    fn user_text_never_panics_on_garbage() {
        for line in ["", "{not json", "{}", r#"{"type":"x"}"#, r#"{"payload":null}"#] {
            assert_eq!(codex_user_text(line), None);
        }
    }

    #[test]
    fn developer_message_is_not_a_model_reply() {
        // A `message` response item with a non-assistant role is input, not the
        // model's reply, so it carries no activity.
        let line = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"x"}]}}"#;
        assert_eq!(codex_activity(line), CodexActivity::Other);
    }

    #[test]
    fn token_count_carries_no_activity() {
        assert_eq!(codex_activity(TOKEN_COUNT_LINE), CodexActivity::Other);
    }

    #[test]
    fn codex_activity_never_panics_on_garbage() {
        for line in ["", "{not json", "{}", r#"{"type":"x"}"#, r#"{"payload":null}"#] {
            assert_eq!(codex_activity(line), CodexActivity::Other);
        }
    }

    // --- Codex live PetState derivation (Q1) --------------------------------

    use crate::model::PetState;

    fn call(id: &str, name: &str) -> CodexActivity {
        CodexActivity::ToolCall {
            call_id: id.into(),
            name: name.into(),
        }
    }
    fn output(id: &str) -> CodexActivity {
        CodexActivity::ToolOutput { call_id: id.into() }
    }

    #[test]
    fn empty_window_is_idle() {
        assert_eq!(derive_codex_state(&[]), PetState::Idle);
    }

    #[test]
    fn trailing_reasoning_is_thinking() {
        assert_eq!(
            derive_codex_state(&[CodexActivity::Message, CodexActivity::Reasoning]),
            PetState::Thinking
        );
    }

    #[test]
    fn unmatched_function_call_is_working_with_name() {
        // A reasoning then a shell call with no output yet -> still working.
        let acts = vec![CodexActivity::Reasoning, call("c1", "shell_command")];
        assert_eq!(
            derive_codex_state(&acts),
            PetState::Working(Some("shell_command".into()))
        );
    }

    #[test]
    fn matched_function_call_then_output_is_thinking() {
        // Tool ran and finished; with no end-turn marker the model is digesting
        // the result -> Thinking (the live state during the reasoning gap).
        let acts = vec![call("c1", "shell_command"), output("c1")];
        assert_eq!(derive_codex_state(&acts), PetState::Thinking);
    }

    #[test]
    fn last_unmatched_call_wins_over_completed_one() {
        let acts = vec![
            call("c1", "read_file"),
            output("c1"),
            call("c2", "apply_patch"),
        ];
        assert_eq!(
            derive_codex_state(&acts),
            PetState::Working(Some("apply_patch".into()))
        );
    }

    #[test]
    fn trailing_message_is_responding() {
        let acts = vec![call("c1", "shell"), output("c1"), CodexActivity::Message];
        assert_eq!(derive_codex_state(&acts), PetState::Responding);
    }

    #[test]
    fn trailing_user_prompt_is_thinking_not_idle() {
        // A fresh turn: the user just submitted and the model has not produced
        // any output yet. This must read Thinking (active), never Idle.
        let acts = vec![CodexActivity::Message, CodexActivity::UserPrompt];
        let st = derive_codex_state(&acts);
        assert_eq!(st, PetState::Thinking);
        assert_ne!(st, PetState::Idle, "a fresh user prompt is an active turn");
    }

    #[test]
    fn tool_call_without_name_is_working_none() {
        let acts = vec![call("c1", "")];
        assert_eq!(derive_codex_state(&acts), PetState::Working(None));
    }

    #[test]
    fn output_before_its_call_does_not_match_future_call() {
        // An output whose call appears AFTER it must not match that later call.
        let acts = vec![output("c1"), call("c1", "shell")];
        assert_eq!(
            derive_codex_state(&acts),
            PetState::Working(Some("shell".into()))
        );
    }

    #[test]
    fn only_other_activities_is_idle() {
        let acts = vec![CodexActivity::Other, CodexActivity::Other];
        assert_eq!(derive_codex_state(&acts), PetState::Idle);
    }

    #[test]
    fn empty_call_id_output_does_not_cancel_empty_call_id_call() {
        // A call + an output both with empty call_id must NOT pair (real Codex
        // always carries a call_id; "" == "" would wrongly cancel the tool).
        let acts = vec![call("", "shell"), output("")];
        assert_eq!(
            derive_codex_state(&acts),
            PetState::Working(Some("shell".into())),
            "empty call_id must be treated as unmatched -> still Working"
        );
    }
}
