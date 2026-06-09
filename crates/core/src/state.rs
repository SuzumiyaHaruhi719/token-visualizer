//! Pet/work-state derivation for a session.
//!
//! Derives the fine-grained work state purely from the tail of the active
//! session's jsonl (recent [`LineKind`]s). This is deliberately a "what does the
//! recent tail SAY?" function — it does NOT consider how old the activity is.
//!
//! Staleness is the POLL LAYER's responsibility: `cmserver::state_poll` (Claude)
//! and [`crate::codex_live`] (Codex) only ever derive state for sessions whose
//! last activity is within [`ACTIVE_WINDOW_MS`]; anything older is dropped before
//! it reaches here. So an active-but-quiet chat (mid-reasoning, not writing every
//! second) keeps its real Working/Thinking state instead of snapping to `Idle`
//! after 60s, and the UI conveys recency via the `updated_at` "Nm ago" label.
//! This mirrors clawd-on-desk, which keeps the last derived state for the whole
//! active window and shows a freshness timestamp rather than forcing Idle.
//!
//! Precedence (highest first):
//! 1. Line-based derivation:
//!    * last unmatched tool call -> [`PetState::Working`] with the tool name
//!      (recognized from both bare `tool_use` lines and usage-bearing assistant
//!      messages whose content is a `tool_use` block — the shape real Claude
//!      jsonl always writes),
//!    * else last meaningful line: reasoning (thinking content) -> Thinking,
//!      end_turn -> Waiting, assistant text -> Responding,
//!    * else (no meaningful line in the tail) -> fall through to (2).
//! 2. `status == "idle"` -> [`PetState::Idle`] (a FALLBACK only, used when the
//!    tail carried no activity signal — never an override of a live line state,
//!    so a finished turn correctly reads `Waiting` not `Idle`).
//! 3. Otherwise -> [`PetState::Idle`].
//!
//! Because real Claude jsonl attaches `message.usage` to every assistant
//! message, the activity is read from the assistant line's content (reasoning /
//! tool / text), NOT from the presence of usage. A pure-reasoning turn therefore
//! maps to [`PetState::Thinking`], never [`PetState::Idle`].

use std::path::Path;

use serde_json::Value;

use crate::model::{AssistantContent, LineKind, PetState, SessionState, Source};

/// Derive the [`PetState`] from a session's coarse status and recent lines.
///
/// Staleness is handled by the poll layer (see the module docs); this function
/// only reads what the recent tail says.
///
/// * `status` — the `status` field from `sessions/<pid>.json` (e.g. `"busy"`).
///   Used only as a fallback to `Idle` when the tail has no activity signal.
/// * `recent_lines` — tail of the session jsonl, oldest-first.
pub fn derive_session_state(status: &str, recent_lines: &[LineKind]) -> PetState {
    // 1. Live line state always wins (an in-window session shows what it's doing).
    if let Some(state) = derive_from_lines(recent_lines) {
        return state;
    }

    // 2. No meaningful activity in the tail: a coarse "idle" status is the only
    //    remaining signal. (Anything else falls through to Idle anyway.)
    if status.eq_ignore_ascii_case("idle") {
        return PetState::Idle;
    }

    // 3. Nothing to go on.
    PetState::Idle
}

/// Build a full [`SessionState`] from session metadata + derived pet state.
///
/// `source` tags which agent the session belongs to (Claude vs Codex) so the
/// merged live list can distinguish them; the Claude poll paths pass
/// [`Source::Claude`]. Codex live sessions are built by `cmcore::codex_live`,
/// which derives its own [`PetState`] and sets [`Source::Codex`].
///
/// `updated_at` is set to `last_activity_ms` (the session's newest line
/// timestamp), NOT the poll time, so the frontend can render "Nm ago" freshness.
/// `last_user_message` is the most recent real user prompt found in
/// `recent_lines` (empty when none is in the scanned tail).
#[allow(clippy::too_many_arguments)]
pub fn session_state_from(
    session_id: &str,
    project: &str,
    model: &str,
    status: &str,
    recent_lines: &[LineKind],
    running_tokens: i64,
    last_activity_ms: i64,
    source: Source,
) -> SessionState {
    let state = derive_session_state(status, recent_lines);
    SessionState {
        session_id: session_id.to_string(),
        project: project.to_string(),
        model: model.to_string(),
        state,
        tokens: running_tokens,
        updated_at: last_activity_ms,
        last_user_message: last_user_message(recent_lines),
        source,
    }
}

/// The most recent real user-prompt text in a window of lines (oldest-first),
/// or empty when none. The parser already filtered out tool results and injected
/// wrappers, so any [`LineKind::UserText`] here is a genuine prompt.
pub fn last_user_message(lines: &[LineKind]) -> String {
    lines
        .iter()
        .rev()
        .find_map(|k| match k {
            LineKind::UserText(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Pure line-based derivation. Returns `None` when the tail carries NO activity
/// signal at all (so the caller can apply a coarse-status fallback), and
/// `Some(state)` otherwise.
fn derive_from_lines(lines: &[LineKind]) -> Option<PetState> {
    // An unmatched tool_use (no later tool_result with the same id) means a
    // tool is still running. Find the LAST such tool_use.
    if let Some(name) = last_unmatched_tool(lines) {
        let tool = if name.is_empty() { None } else { Some(name) };
        return Some(PetState::Working(tool));
    }

    // Otherwise inspect the last meaningful (non-Other) line. Reasoning wins as
    // an ACTIVE state: a finalized assistant message whose only content is a
    // `thinking` block (real Claude jsonl) means the model was reasoning, not
    // idling — so it must map to Thinking, never Idle.
    for kind in lines.iter().rev() {
        match kind {
            LineKind::Thinking => return Some(PetState::Thinking),
            LineKind::EndTurn => return Some(PetState::Waiting),
            LineKind::Assistant { content, .. } => return Some(state_for_content(content)),
            // A trailing tool_result (its tool_use already matched, handled
            // above) means the model has just received the tool output and is
            // reasoning about it before its next step. Empirically this is the
            // line that sits at the tail of the jsonl for the entire 15-60s
            // extended-reasoning window (Claude writes the finalized `thinking`
            // block only AFTER reasoning completes), so mapping it to Thinking —
            // not Responding — is what makes the live "thinking" state actually
            // appear during that gap.
            LineKind::ToolResult { .. } => return Some(PetState::Thinking),
            LineKind::ToolUse { .. } => {
                // Matched tool_use (unmatched handled above) -> keep scanning.
                continue;
            }
            // A user prompt with no later assistant activity means the model has
            // not started responding yet — it is about to work. Treat a trailing
            // user prompt as Thinking (an active state), never a stale Idle.
            LineKind::UserText(_) => return Some(PetState::Thinking),
            LineKind::Other => continue,
        }
    }

    None
}

/// Map the content of a usage-bearing assistant line to a pet state. Tool calls
/// are handled by the pairing pass (so a matched tool falls through to here only
/// when it already completed) — a matched tool message means the model has moved
/// on to its next step, so treat a trailing tool message as responding.
fn state_for_content(content: &AssistantContent) -> PetState {
    match content {
        AssistantContent::Thinking => PetState::Thinking,
        AssistantContent::Tool { .. } => PetState::Responding,
        AssistantContent::Text => PetState::Responding,
    }
}

/// Return the tool name of the last tool call that has no matching `tool_result`
/// appearing after it in the window. Recognizes a tool call from BOTH a bare
/// `tool_use` line (streaming) and a usage-bearing assistant message whose
/// content is a `tool_use` block (the shape real Claude jsonl always writes).
fn last_unmatched_tool(lines: &[LineKind]) -> Option<String> {
    // For each tool call (scanning from the end), it is unmatched if no
    // tool_result with the same id occurs AFTER its position.
    for (i, kind) in lines.iter().enumerate().rev() {
        let Some((id, name)) = tool_call(kind) else {
            continue;
        };
        let matched_after = lines[i + 1..]
            .iter()
            .any(|k| matches!(k, LineKind::ToolResult { tool_use_id } if tool_use_id == id));
        if !matched_after {
            return Some(name.to_string());
        }
    }
    None
}

/// Extract `(tool_use_id, tool_name)` from any line that represents a tool call,
/// whether bare (`ToolUse`) or carried on a usage-bearing assistant message.
fn tool_call(kind: &LineKind) -> Option<(&str, &str)> {
    match kind {
        LineKind::ToolUse { id, name } => Some((id, name)),
        LineKind::Assistant {
            content: AssistantContent::Tool { id, name },
            ..
        } => Some((id, name)),
        _ => None,
    }
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
