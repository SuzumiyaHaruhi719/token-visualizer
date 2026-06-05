//! Pet/work-state derivation for a session.
//!
//! Combines the coarse `status` from `sessions/<pid>.json` with fine-grained
//! signals from the tail of the active session's jsonl (recent [`LineKind`]s).
//!
//! Precedence (highest first):
//! 1. Heartbeat staleness — very stale -> [`PetState::Sleeping`], stale ->
//!    [`PetState::Idle`] (an inactive session is inactive regardless of status).
//! 2. `status == "idle"` -> [`PetState::Idle`].
//! 3. Line-based derivation (also the fallback for unknown statuses):
//!    * last unmatched `tool_use` -> [`PetState::Working`] with the tool name
//!    * else last meaningful line: thinking -> Thinking, end_turn -> Waiting,
//!      assistant text -> Responding
//!    * else -> [`PetState::Idle`].

use std::path::Path;

use serde_json::Value;

use crate::model::{LineKind, PetState, SessionState};

/// Below this idle gap we still trust the live line state.
pub const IDLE_MS: i64 = 60_000;
/// Above this gap the pet goes to sleep.
pub const SLEEP_MS: i64 = 600_000;

/// Derive the [`PetState`] from a session's coarse status and recent lines.
///
/// * `status` — the `status` field from `sessions/<pid>.json` (e.g. `"busy"`).
/// * `recent_lines` — tail of the session jsonl, oldest-first.
/// * `running_tokens` — running token total (unused for state, kept for parity
///   with the session helper / future heuristics).
/// * `now_ms` / `last_activity_ms` — for heartbeat staleness.
pub fn derive_session_state(
    status: &str,
    recent_lines: &[LineKind],
    _running_tokens: i64,
    now_ms: i64,
    last_activity_ms: i64,
) -> PetState {
    let idle_for = now_ms.saturating_sub(last_activity_ms);

    // 1. Staleness wins regardless of reported status.
    if idle_for > SLEEP_MS {
        return PetState::Sleeping;
    }
    if idle_for > IDLE_MS {
        return PetState::Idle;
    }

    // 2. Explicit idle status.
    if status.eq_ignore_ascii_case("idle") {
        return PetState::Idle;
    }

    // 3. Line-based (busy or unknown status both land here).
    derive_from_lines(recent_lines)
}

/// Build a full [`SessionState`] from session metadata + derived pet state.
#[allow(clippy::too_many_arguments)]
pub fn session_state_from(
    session_id: &str,
    project: &str,
    model: &str,
    status: &str,
    recent_lines: &[LineKind],
    running_tokens: i64,
    now_ms: i64,
    last_activity_ms: i64,
) -> SessionState {
    let state = derive_session_state(
        status,
        recent_lines,
        running_tokens,
        now_ms,
        last_activity_ms,
    );
    SessionState {
        session_id: session_id.to_string(),
        project: project.to_string(),
        model: model.to_string(),
        state,
        tokens: running_tokens,
        updated_at: now_ms,
    }
}

/// Pure line-based derivation (used when the session is live and not idle).
fn derive_from_lines(lines: &[LineKind]) -> PetState {
    // An unmatched tool_use (no later tool_result with the same id) means a
    // tool is still running. Find the LAST such tool_use.
    if let Some(name) = last_unmatched_tool(lines) {
        let tool = if name.is_empty() { None } else { Some(name) };
        return PetState::Working(tool);
    }

    // Otherwise inspect the last meaningful (non-Other) line.
    for kind in lines.iter().rev() {
        match kind {
            LineKind::Thinking => return PetState::Thinking,
            LineKind::EndTurn => return PetState::Waiting,
            LineKind::Assistant(_) => return PetState::Responding,
            // A bare tool_result with no surrounding context: treat as still
            // working-ish? No — pairing already handled tool_use. A trailing
            // tool_result implies the model is about to respond.
            LineKind::ToolResult { .. } => return PetState::Responding,
            LineKind::ToolUse { .. } => {
                // Matched tool_use (unmatched handled above) -> keep scanning.
                continue;
            }
            LineKind::Other => continue,
        }
    }

    PetState::Idle
}

/// Return the tool name of the last `tool_use` that has no matching
/// `tool_result` appearing after it in the window.
fn last_unmatched_tool(lines: &[LineKind]) -> Option<String> {
    // Collect all tool_result ids seen.
    // For each tool_use (scanning from the end), it is unmatched if no
    // tool_result with the same id occurs AFTER its position.
    for (i, kind) in lines.iter().enumerate().rev() {
        if let LineKind::ToolUse { id, name } = kind {
            let matched_after = lines[i + 1..]
                .iter()
                .any(|k| matches!(k, LineKind::ToolResult { tool_use_id } if tool_use_id == id));
            if !matched_after {
                return Some(name.clone());
            }
        }
    }
    None
}

/// The coarse status read from a `sessions/<pid>.json` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStatus {
    /// Raw `status` string (e.g. `"busy"`, `"idle"`); empty if absent.
    pub status: String,
    /// Last heartbeat in epoch millis, or 0 if absent.
    pub updated_at: i64,
    /// Session id if present in the file.
    pub session_id: Option<String>,
}

/// Parse a `sessions/<pid>.json` blob into a [`SessionStatus`]. Tolerant:
/// missing/garbage fields default rather than error. `updatedAt` may be an
/// ISO-8601 string or epoch millis.
pub fn parse_session_status(json: &str) -> SessionStatus {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return SessionStatus::default(),
    };
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let updated_at = match v.get("updatedAt") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0),
        _ => 0,
    };
    let session_id = v
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    SessionStatus {
        status,
        updated_at,
        session_id,
    }
}

/// Read & parse a session status file from disk (read-only). Returns the
/// default status if the file can't be read or parsed.
pub fn read_session_status(path: &Path) -> SessionStatus {
    match std::fs::read_to_string(path) {
        Ok(s) => parse_session_status(&s),
        Err(_) => SessionStatus::default(),
    }
}
