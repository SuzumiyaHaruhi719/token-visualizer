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
}
