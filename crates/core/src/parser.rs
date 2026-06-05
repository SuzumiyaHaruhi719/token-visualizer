//! Tolerant jsonl line parser: one line -> [`LineKind`].
//!
//! Every error path collapses to [`LineKind::Other`]; this function NEVER
//! panics on invalid, partial, or unexpected input. Half-written JSON (the tail
//! of a file being appended to) parses as `Other`, which the importer/watcher
//! treat as "do not advance the offset".

use chrono::DateTime;
use serde_json::Value;

use crate::model::{LineKind, ParsedEvent, Usage};
use crate::paths::project_name_from_cwd;

/// Parse a single jsonl line into a [`LineKind`]. Tolerant: any failure yields
/// [`LineKind::Other`].
pub fn parse_line(line: &str) -> LineKind {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return LineKind::Other,
    };
    classify(&v)
}

/// Classify an already-parsed JSON value.
fn classify(v: &Value) -> LineKind {
    let message = v.get("message");

    // 1. Assistant message carrying usage -> the only thing we bill on.
    if let Some(usage_val) = message.and_then(|m| m.get("usage")) {
        if usage_val.is_object() {
            return LineKind::Assistant(build_event(v, usage_val));
        }
    }

    // 2. Inspect content blocks for tool_use / tool_result / thinking.
    if let Some(content) = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        // tool_use takes priority (it determines "working").
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let id = str_field(block, "id").unwrap_or_default();
                let name = str_field(block, "name").unwrap_or_default();
                return LineKind::ToolUse { id, name };
            }
        }
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                let tool_use_id = str_field(block, "tool_use_id").unwrap_or_default();
                return LineKind::ToolResult { tool_use_id };
            }
        }
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("thinking") {
                return LineKind::Thinking;
            }
        }
    }

    // 3. end_turn (no tool_use found above).
    if message
        .and_then(|m| m.get("stop_reason"))
        .and_then(Value::as_str)
        == Some("end_turn")
    {
        return LineKind::EndTurn;
    }

    LineKind::Other
}

/// Build a [`ParsedEvent`] from an assistant value and its `usage` object.
fn build_event(v: &Value, usage_val: &Value) -> ParsedEvent {
    let message = v.get("message");

    let request_id = str_field(v, "requestId")
        .or_else(|| str_field(v, "uuid"))
        .unwrap_or_default();

    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso_millis)
        .unwrap_or(0);

    let session_id = str_field(v, "sessionId").unwrap_or_default();

    let project = v
        .get("cwd")
        .and_then(Value::as_str)
        .map(project_name_from_cwd)
        .unwrap_or_else(|| "unknown".to_string());

    let model = message
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    ParsedEvent {
        request_id,
        ts,
        session_id,
        project,
        model,
        usage: parse_usage(usage_val),
    }
}

/// Extract the token-usage fields, defaulting any missing field to 0.
fn parse_usage(usage: &Value) -> Usage {
    let server = usage.get("server_tool_use");
    Usage {
        input: int_field(usage, "input_tokens"),
        output: int_field(usage, "output_tokens"),
        cache_create: int_field(usage, "cache_creation_input_tokens"),
        cache_read: int_field(usage, "cache_read_input_tokens"),
        web_search: server
            .map(|s| int_field(s, "web_search_requests"))
            .unwrap_or(0),
        web_fetch: server
            .map(|s| int_field(s, "web_fetch_requests"))
            .unwrap_or(0),
    }
}

/// Parse an ISO-8601 timestamp into epoch milliseconds (UTC).
fn parse_iso_millis(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Read an integer field, tolerating ints, floats, and numeric strings.
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
