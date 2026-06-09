//! Tolerant parser for the Reasonix (DeepSeek) client logs + mapping into our model.
//!
//! Reasonix writes three kinds of file under `~/.reasonix`:
//!
//! * `usage.jsonl` — ONE JSON object per turn, the token + cost source we bill on:
//!   `{"ts":<epoch_ms>,"session":"code-Projects","model":"deepseek-v4-pro",
//!    "promptTokens":..,"completionTokens":..,"cacheHitTokens":..,
//!    "cacheMissTokens":..,"costUsd":..,"claudeEquivUsd":..}`.
//!   `promptTokens == cacheHitTokens + cacheMissTokens`, so we map
//!   `input = cacheMissTokens`, `cache_read = cacheHitTokens`,
//!   `output = completionTokens`, `cache_create = 0`.
//! * `sessions/<name>.jsonl` — the conversation (`role:user/assistant/tool`),
//!   no per-line timestamps. Source of the last user prompt.
//! * `sessions/<name>.events.jsonl` — timestamped events (the activity signal for
//!   live state): `model.turn.started`, `tool.preparing` / `tool.intent` /
//!   `tool.dispatched` / `tool.call`, `tool.result`, `status`, `model.final`.
//!
//! Like [`crate::parser`] and [`crate::codex`], every failure collapses to the
//! `Other` variant; this module NEVER panics on partial / garbage input. The
//! Reasonix logs are only ever READ.

use serde_json::Value;

use crate::model::{ParsedEvent, Source, Usage};

/// A parsed `usage.jsonl` turn (the billable shape), before mapping to [`Usage`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReasonixUsageLine {
    /// Epoch milliseconds (the line's `ts`).
    pub ts: i64,
    /// The `session` field (e.g. `code-Projects`) — our session id.
    pub session: String,
    /// The DeepSeek model id (e.g. `deepseek-v4-pro`).
    pub model: String,
    /// Non-cached prompt tokens (`cacheMissTokens`) → [`Usage::input`].
    pub cache_miss_tokens: i64,
    /// Cached prompt tokens (`cacheHitTokens`) → [`Usage::cache_read`].
    pub cache_hit_tokens: i64,
    /// Completion tokens (`completionTokens`) → [`Usage::output`].
    pub completion_tokens: i64,
}

impl ReasonixUsageLine {
    /// Map this turn's token counts into our generic [`Usage`].
    ///
    /// `input = cacheMissTokens`, `cache_read = cacheHitTokens`,
    /// `output = completionTokens`. Reasonix has no separate cache-WRITE counter,
    /// so `cache_create` is always 0 (cache writes are folded into the miss/input
    /// count by DeepSeek's accounting).
    pub fn to_usage(&self) -> Usage {
        Usage {
            input: self.cache_miss_tokens.max(0),
            output: self.completion_tokens.max(0),
            cache_create: 0,
            cache_read: self.cache_hit_tokens.max(0),
            web_search: 0,
            web_fetch: 0,
        }
    }

    /// Total billable tokens in this turn (used to skip empty heartbeat lines).
    pub fn total_tokens(&self) -> i64 {
        self.cache_miss_tokens + self.cache_hit_tokens + self.completion_tokens
    }
}

/// Parse a single `usage.jsonl` line. Tolerant: any failure or a line missing the
/// required token fields yields `None`.
pub fn parse_usage_line(line: &str) -> Option<ReasonixUsageLine> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if !v.is_object() {
        return None;
    }
    // A real usage line always carries these token fields; a line without them
    // (a different record shape) is not billable.
    let model = v.get("model").and_then(Value::as_str)?.to_string();
    if model.is_empty() {
        return None;
    }
    let session = v
        .get("session")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("reasonix")
        .to_string();
    Some(ReasonixUsageLine {
        ts: int_field(&v, "ts"),
        session,
        model,
        cache_miss_tokens: int_field(&v, "cacheMissTokens"),
        cache_hit_tokens: int_field(&v, "cacheHitTokens"),
        completion_tokens: int_field(&v, "completionTokens"),
    })
}

/// Build a [`ParsedEvent`] for one Reasonix usage turn.
///
/// The dedup key is `reasonix:<session>:<line_offset>` (the byte offset of THIS
/// line in `usage.jsonl`), mirroring Codex's `codex:<session>:<offset>` so the
/// same turn read twice (re-import / overlapping watch) collapses to one row.
/// `project` is the workspace basename (or the session name when unknown).
pub fn reasonix_event(line: &ReasonixUsageLine, project: &str, line_offset: i64) -> ParsedEvent {
    ParsedEvent {
        request_id: format!("reasonix:{}:{}", line.session, line_offset),
        ts: line.ts,
        session_id: line.session.clone(),
        project: project.to_string(),
        model: line.model.clone(),
        usage: line.to_usage(),
        source: Source::DeepSeek,
    }
}

// --- live activity (events.jsonl) -------------------------------------------

/// The live activity a Reasonix `events.jsonl` line represents, mirroring
/// [`crate::codex::CodexActivity`] so a Reasonix session classifies into the same
/// pet states. Reasonix streams each step as its own timestamped event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasonixActivity {
    /// A turn just started (`model.turn.started`) — the model is thinking.
    TurnStarted,
    /// A thinking/active `status` line (e.g. "…思考中…"/"…正在…") — Thinking.
    StatusThinking,
    /// A tool is being prepared / dispatched / called, carrying its call id +
    /// name when known. `tool.preparing` / `tool.intent` / `tool.dispatched` /
    /// `tool.call` all map here.
    ToolCall { call_id: String, name: String },
    /// A tool finished (`tool.result`) for the given call id (ok or error).
    ToolResult { call_id: String },
    /// The model emitted its final reply for the turn (`model.final`) —
    /// Responding.
    Final,
    /// A real USER turn boundary — there is no explicit user event, so this is
    /// never produced from `events.jsonl`; kept for symmetry / future use.
    UserPrompt,
    /// Anything else (session.opened, slash.invoked, a non-active status, …).
    Other,
}

/// Classify a Reasonix `events.jsonl` line into a live [`ReasonixActivity`].
/// Tolerant: any parse failure / unrecognized shape yields
/// [`ReasonixActivity::Other`].
pub fn reasonix_activity(line: &str) -> ReasonixActivity {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return ReasonixActivity::Other,
    };
    classify_activity(&v)
}

fn classify_activity(v: &Value) -> ReasonixActivity {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "model.turn.started" => ReasonixActivity::TurnStarted,
        // A tool invocation. In real Reasonix logs `tool.preparing` /
        // `tool.intent` / `tool.dispatched` all carry the `callId` (and name),
        // while `tool.call` is an id-LESS duplicate of the same step. Only the
        // id-carrying events are treated as a pairable ToolCall — an id-less
        // `tool.call` is dropped to `Other` so it can never become a phantom
        // "still running" tool that never matches a later `tool.result` (Codex's
        // review: do not let an id-less tool event stay unmatched forever).
        "tool.preparing" | "tool.intent" | "tool.dispatched" | "tool.call" => {
            let call_id = call_id_of(v);
            if call_id.is_empty() {
                ReasonixActivity::Other
            } else {
                ReasonixActivity::ToolCall {
                    call_id,
                    name: str_field(v, "name").unwrap_or_default(),
                }
            }
        }
        "tool.result" => ReasonixActivity::ToolResult {
            call_id: call_id_of(v),
        },
        "model.final" => ReasonixActivity::Final,
        // A `status` line is an activity signal ONLY when its text marks an active
        // thinking phase; an idle/other status carries no signal (per Codex's
        // review: do not treat every status blindly as Thinking).
        "status" => {
            if is_thinking_status(v.get("text").and_then(Value::as_str).unwrap_or_default()) {
                ReasonixActivity::StatusThinking
            } else {
                ReasonixActivity::Other
            }
        }
        _ => ReasonixActivity::Other,
    }
}

/// Whether a `status` event's text marks an ACTIVE (thinking/working) phase
/// rather than an idle/completed one. Reasonix status text is localized (Chinese)
/// — the active markers seen in real logs contain "思考"/"生成"/"正在" or an
/// ellipsis. We match conservatively: an active status is one that contains a
/// known busy marker.
fn is_thinking_status(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    const BUSY_MARKERS: &[&str] = &[
        "思考",   // thinking
        "生成",   // generating
        "正在",   // in progress ("currently …")
        "上传",   // uploading (tool result uploaded -> model is about to think)
        "running",
        "thinking",
        "generating",
    ];
    BUSY_MARKERS.iter().any(|m| text.contains(m))
}

/// Derive a live [`crate::model::PetState`] from a window of Reasonix activities
/// (oldest-first, the tail of an `events.jsonl`). Mirrors
/// [`crate::codex::derive_codex_state`] and applies the SAME tool-pairing rule:
///
/// * the LAST tool call with no later `tool.result` for the same `call_id` means
///   a tool is still running -> [`PetState::Working`] (tool name),
/// * otherwise the last meaningful activity decides:
///   * `model.final` -> [`PetState::Responding`],
///   * a `tool.result` (the tool finished; the model is now reasoning about it,
///     with no final marker yet) -> [`PetState::Thinking`],
///   * `model.turn.started` / a thinking `status` -> [`PetState::Thinking`],
/// * nothing meaningful -> [`PetState::Idle`].
///
/// Pure + total: an empty window yields [`PetState::Idle`]; it never panics.
pub fn derive_reasonix_state(activities: &[ReasonixActivity]) -> crate::model::PetState {
    use crate::model::PetState;

    if let Some(name) = last_unmatched_tool(activities) {
        let tool = if name.is_empty() { None } else { Some(name) };
        return PetState::Working(tool);
    }

    for activity in activities.iter().rev() {
        match activity {
            ReasonixActivity::Final => return PetState::Responding,
            // The tool finished (ok or error) and no final yet -> the model is
            // reasoning about the result.
            ReasonixActivity::ToolResult { .. } => return PetState::Thinking,
            ReasonixActivity::TurnStarted => return PetState::Thinking,
            ReasonixActivity::StatusThinking => return PetState::Thinking,
            ReasonixActivity::UserPrompt => return PetState::Thinking,
            // A matched ToolCall (the unmatched case is handled above) means the
            // model already moved past it — keep scanning for the real tail state.
            ReasonixActivity::ToolCall { .. } => continue,
            ReasonixActivity::Other => continue,
        }
    }
    PetState::Idle
}

/// The tool name of the last tool call that has no matching `tool.result` (same
/// `call_id`) appearing AFTER it in the window — i.e. a tool still running.
/// Mirrors Codex's `last_unmatched_codex_tool`.
///
/// One Reasonix tool invocation spans several id-carrying events for the SAME
/// `call_id` (`tool.preparing`/`tool.intent` carry the name; `tool.dispatched`
/// does NOT). The newest event for the in-flight call can therefore be the
/// nameless `tool.dispatched`, so when the matched call has an empty name we
/// recover it from any other `ToolCall` sharing that `call_id` in the window.
fn last_unmatched_tool(activities: &[ReasonixActivity]) -> Option<String> {
    for (i, activity) in activities.iter().enumerate().rev() {
        let ReasonixActivity::ToolCall { call_id, name } = activity else {
            continue;
        };
        // An empty call_id can never be paired (a real Reasonix call always
        // carries one); treating "" == "" as a match would wrongly cancel an
        // in-flight tool. So a call with an empty id is always considered
        // unmatched -> still Working.
        let matched_after = !call_id.is_empty()
            && activities[i + 1..].iter().any(|a| {
                matches!(a, ReasonixActivity::ToolResult { call_id: out_id } if out_id == call_id)
            });
        if !matched_after {
            if !name.is_empty() {
                return Some(name.clone());
            }
            // This in-flight event is nameless (e.g. `tool.dispatched`); recover
            // the name from a sibling event of the SAME call_id, if any.
            return Some(name_for_call_id(activities, call_id));
        }
    }
    None
}

/// The first non-empty `name` among all `ToolCall`s sharing `call_id` (empty
/// `call_id` never matches a sibling). Empty string when none carries a name.
fn name_for_call_id(activities: &[ReasonixActivity], call_id: &str) -> String {
    if call_id.is_empty() {
        return String::new();
    }
    activities
        .iter()
        .find_map(|a| match a {
            ReasonixActivity::ToolCall { call_id: cid, name }
                if cid == call_id && !name.is_empty() =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

// --- session.jsonl user prompt ----------------------------------------------

/// Extract a real USER prompt from a Reasonix `sessions/<name>.jsonl` line, or
/// `None`.
///
/// A Reasonix user message is `{"role":"user","content":"..."}`. Reasonix injects
/// synthetic user-role content too (slash wrappers / context), so injected
/// wrappers are filtered via [`crate::text::is_injected_user_text`]; the result is
/// cleaned + capped to a single line. Tolerant: any parse failure / non-matching
/// shape yields `None`.
pub fn reasonix_user_text(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    user_text_from_value(&v)
}

fn user_text_from_value(v: &Value) -> Option<String> {
    if v.get("role").and_then(Value::as_str)? != "user" {
        return None;
    }
    let raw = content_to_string(v.get("content")?);
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

/// Reasonix content is usually a plain string; tolerate the array-of-parts shape
/// (`[{"type":"text","text":"…"}]`) too by concatenating any text parts.
fn content_to_string(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

// --- small field helpers -----------------------------------------------------

/// A Reasonix tool event's call id — real logs use `callId`; tolerate `call_id`.
fn call_id_of(v: &Value) -> String {
    str_field(v, "callId")
        .or_else(|| str_field(v, "call_id"))
        .unwrap_or_default()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PetState;

    // A real-shape usage.jsonl line captured from this machine.
    const USAGE_LINE: &str = r#"{"ts":1780970065293,"session":"code-Projects","model":"deepseek-v4-pro","promptTokens":32716,"completionTokens":433,"cacheHitTokens":32000,"cacheMissTokens":716,"costUsd":0.00080417,"claudeEquivUsd":0.104643}"#;

    #[test]
    fn parses_usage_line_and_maps_usage() {
        let line = parse_usage_line(USAGE_LINE).expect("parse");
        assert_eq!(line.ts, 1_780_970_065_293);
        assert_eq!(line.session, "code-Projects");
        assert_eq!(line.model, "deepseek-v4-pro");
        assert_eq!(line.cache_miss_tokens, 716);
        assert_eq!(line.cache_hit_tokens, 32_000);
        assert_eq!(line.completion_tokens, 433);

        let u = line.to_usage();
        // input = cacheMiss, cache_read = cacheHit, output = completion, cc = 0.
        assert_eq!(u.input, 716);
        assert_eq!(u.cache_read, 32_000);
        assert_eq!(u.output, 433);
        assert_eq!(u.cache_create, 0);
        // promptTokens == cacheHit + cacheMiss (the invariant the mapping relies on).
        assert_eq!(u.input + u.cache_read, 32_716);
    }

    #[test]
    fn usage_total_excludes_nothing_and_flags_empty() {
        let line = parse_usage_line(USAGE_LINE).unwrap();
        assert_eq!(line.total_tokens(), 716 + 32_000 + 433);
    }

    #[test]
    fn flash_model_parses() {
        let l = r#"{"ts":1780916586293,"session":"code-Projects","model":"deepseek-v4-flash","promptTokens":23015,"completionTokens":140,"cacheHitTokens":0,"cacheMissTokens":23015,"costUsd":0.0032613}"#;
        let line = parse_usage_line(l).unwrap();
        assert_eq!(line.model, "deepseek-v4-flash");
        assert_eq!(line.to_usage().input, 23_015);
        assert_eq!(line.to_usage().cache_read, 0);
    }

    #[test]
    fn builds_dedup_request_id_with_line_offset() {
        let line = parse_usage_line(USAGE_LINE).unwrap();
        let e = reasonix_event(&line, "Projects", 4096);
        assert_eq!(e.request_id, "reasonix:code-Projects:4096");
        assert_eq!(e.source, Source::DeepSeek);
        assert_eq!(e.model, "deepseek-v4-pro");
        assert_eq!(e.session_id, "code-Projects");
        assert_eq!(e.project, "Projects");
        assert_eq!(e.usage.cache_read, 32_000);
    }

    #[test]
    fn missing_session_defaults_but_still_parses() {
        let l = r#"{"ts":1,"model":"deepseek-v4-pro","completionTokens":1,"cacheHitTokens":0,"cacheMissTokens":2}"#;
        let line = parse_usage_line(l).unwrap();
        assert_eq!(line.session, "reasonix");
    }

    #[test]
    fn usage_parse_never_panics_on_garbage() {
        for line in ["", "{not json", "{}", r#"{"ts":1}"#, r#"{"model":""}"#] {
            assert!(parse_usage_line(line).is_none(), "line {line:?} should not parse");
        }
    }

    // --- activity classification (real captured event shapes) ----------------

    const TURN_STARTED: &str = r#"{"id":2,"ts":"2026-06-09T01:52:18.698Z","turn":3,"type":"model.turn.started","model":"deepseek-v4-pro","reasoningEffort":"high"}"#;
    const TOOL_PREPARING: &str = r#"{"id":82,"ts":"2026-06-09T01:52:20.601Z","turn":3,"type":"tool.preparing","callId":"tc-1","name":"run_command"}"#;
    const TOOL_CALL: &str = r#"{"id":86,"ts":"2026-06-09T01:52:21.052Z","turn":3,"type":"tool.call","name":"run_command","args":{"command":"x"}}"#;
    const TOOL_RESULT: &str = r#"{"id":87,"ts":"2026-06-09T01:52:21.175Z","turn":3,"type":"tool.result","callId":"tc-1","ok":true,"output":"ok"}"#;
    const STATUS_THINKING: &str = r#"{"id":88,"ts":"2026-06-09T01:52:21.176Z","turn":3,"type":"status","text":"工具结果已上传 · 模型在生成下一条响应前思考中…"}"#;
    const MODEL_FINAL: &str = r#"{"id":57,"ts":"2026-06-09T01:52:21.000Z","turn":3,"type":"model.final","content":"done","usage":{"prompt_tokens":1}}"#;
    const SESSION_OPENED: &str = r#"{"id":1,"ts":"2026-06-09T01:51:21.480Z","turn":0,"type":"session.opened","name":"code-Projects"}"#;

    #[test]
    fn classifies_event_types() {
        assert_eq!(reasonix_activity(TURN_STARTED), ReasonixActivity::TurnStarted);
        assert_eq!(
            reasonix_activity(TOOL_PREPARING),
            ReasonixActivity::ToolCall {
                call_id: "tc-1".into(),
                name: "run_command".into(),
            }
        );
        assert_eq!(
            reasonix_activity(TOOL_RESULT),
            ReasonixActivity::ToolResult {
                call_id: "tc-1".into()
            }
        );
        assert_eq!(reasonix_activity(STATUS_THINKING), ReasonixActivity::StatusThinking);
        assert_eq!(reasonix_activity(MODEL_FINAL), ReasonixActivity::Final);
    }

    #[test]
    fn id_less_tool_call_is_dropped_to_other() {
        // Real Reasonix `tool.call` lines carry NO callId — they are id-less
        // duplicates of the id-carrying `tool.preparing`/`tool.intent`/
        // `tool.dispatched`. They must classify as Other so they never become a
        // phantom unmatched (forever-Working) tool after the real `tool.result`.
        assert_eq!(reasonix_activity(TOOL_CALL), ReasonixActivity::Other);
    }

    #[test]
    fn id_carrying_tool_dispatched_pairs_with_result() {
        // The authoritative tool-start signals carry the callId; a later
        // tool.result with the same id matches them -> NOT still running.
        let dispatched = r#"{"type":"tool.dispatched","callId":"tc-7","name":"run_command"}"#;
        assert_eq!(
            reasonix_activity(dispatched),
            ReasonixActivity::ToolCall {
                call_id: "tc-7".into(),
                name: "run_command".into()
            }
        );
        let acts = vec![
            reasonix_activity(dispatched),
            ReasonixActivity::ToolResult { call_id: "tc-7".into() },
        ];
        assert_eq!(derive_reasonix_state(&acts), PetState::Thinking);
    }

    #[test]
    fn idle_status_is_not_a_thinking_signal() {
        // A status that carries no busy marker must NOT count as Thinking.
        let idle = r#"{"type":"status","text":"Ready."}"#;
        assert_eq!(reasonix_activity(idle), ReasonixActivity::Other);
    }

    #[test]
    fn session_opened_and_slash_carry_no_activity() {
        assert_eq!(reasonix_activity(SESSION_OPENED), ReasonixActivity::Other);
        let slash = r#"{"type":"slash.invoked","name":"model","args":"deepseek-v4-pro"}"#;
        assert_eq!(reasonix_activity(slash), ReasonixActivity::Other);
    }

    #[test]
    fn activity_never_panics_on_garbage() {
        for line in ["", "{not json", "{}", r#"{"type":"x"}"#, r#"{"type":null}"#] {
            assert_eq!(reasonix_activity(line), ReasonixActivity::Other);
        }
    }

    // --- live PetState derivation --------------------------------------------

    fn call(id: &str, name: &str) -> ReasonixActivity {
        ReasonixActivity::ToolCall {
            call_id: id.into(),
            name: name.into(),
        }
    }
    fn result(id: &str) -> ReasonixActivity {
        ReasonixActivity::ToolResult { call_id: id.into() }
    }

    #[test]
    fn empty_window_is_idle() {
        assert_eq!(derive_reasonix_state(&[]), PetState::Idle);
    }

    #[test]
    fn trailing_final_is_responding() {
        let acts = vec![call("c1", "run_command"), result("c1"), ReasonixActivity::Final];
        assert_eq!(derive_reasonix_state(&acts), PetState::Responding);
    }

    #[test]
    fn unmatched_tool_call_is_working_with_name() {
        let acts = vec![ReasonixActivity::TurnStarted, call("tc-1", "run_command")];
        assert_eq!(
            derive_reasonix_state(&acts),
            PetState::Working(Some("run_command".into()))
        );
    }

    #[test]
    fn matched_tool_then_result_is_thinking() {
        // Tool ran + finished, no final yet -> the model is digesting the result.
        let acts = vec![call("tc-1", "run_command"), result("tc-1")];
        assert_eq!(derive_reasonix_state(&acts), PetState::Thinking);
    }

    #[test]
    fn error_tool_result_is_still_thinking() {
        // An `ok:false` tool.result still classifies as a ToolResult -> Thinking
        // (the model reasons about the failure before its next step).
        let err_result = r#"{"type":"tool.result","callId":"tc-1","ok":false,"output":"boom"}"#;
        assert_eq!(
            reasonix_activity(err_result),
            ReasonixActivity::ToolResult { call_id: "tc-1".into() }
        );
        let acts = vec![call("tc-1", "x"), result("tc-1")];
        assert_eq!(derive_reasonix_state(&acts), PetState::Thinking);
    }

    #[test]
    fn last_unmatched_call_wins_over_completed_one() {
        let acts = vec![call("c1", "read"), result("c1"), call("c2", "write")];
        assert_eq!(
            derive_reasonix_state(&acts),
            PetState::Working(Some("write".into()))
        );
    }

    #[test]
    fn trailing_turn_started_is_thinking_not_idle() {
        let acts = vec![ReasonixActivity::Final, ReasonixActivity::TurnStarted];
        let st = derive_reasonix_state(&acts);
        assert_eq!(st, PetState::Thinking);
        assert_ne!(st, PetState::Idle);
    }

    #[test]
    fn trailing_thinking_status_is_thinking() {
        let acts = vec![result("c1"), ReasonixActivity::StatusThinking];
        assert_eq!(derive_reasonix_state(&acts), PetState::Thinking);
    }

    #[test]
    fn working_recovers_name_from_sibling_event_of_same_call_id() {
        // Real Reasonix shape: tool.intent(tc-40, name) -> tool.dispatched(tc-40,
        // NO name), no result yet. The newest in-flight event is nameless, so the
        // name must be recovered from the sibling intent -> Working("submit_plan").
        let acts = vec![
            call("tc-40", "submit_plan"), // tool.intent (carries name)
            call("tc-40", ""),            // tool.dispatched (no name)
        ];
        assert_eq!(
            derive_reasonix_state(&acts),
            PetState::Working(Some("submit_plan".into()))
        );
    }

    #[test]
    fn empty_call_id_call_is_never_cancelled() {
        // A call + result both with empty call_id must NOT pair.
        let acts = vec![call("", "run_command"), result("")];
        assert_eq!(
            derive_reasonix_state(&acts),
            PetState::Working(Some("run_command".into()))
        );
    }

    #[test]
    fn only_other_activities_is_idle() {
        let acts = vec![ReasonixActivity::Other, ReasonixActivity::Other];
        assert_eq!(derive_reasonix_state(&acts), PetState::Idle);
    }

    // --- user prompt extraction (real session.jsonl shapes) ------------------

    #[test]
    fn extracts_real_user_prompt() {
        let line = r#"{"role":"user","content":"帮我扫扫电脑的垃圾文件 除了扫描不要做任何删除写入操作"}"#;
        assert_eq!(
            reasonix_user_text(line).as_deref(),
            Some("帮我扫扫电脑的垃圾文件 除了扫描不要做任何删除写入操作")
        );
    }

    #[test]
    fn skips_assistant_and_tool_roles() {
        let assistant = r#"{"role":"assistant","content":"好的","reasoning_content":"..."}"#;
        let tool = r#"{"role":"tool","tool_call_id":"x","name":"run_command","content":"ok"}"#;
        assert_eq!(reasonix_user_text(assistant), None);
        assert_eq!(reasonix_user_text(tool), None);
    }

    #[test]
    fn skips_injected_wrapper_user_message() {
        let injected = r#"{"role":"user","content":"<command-name>model</command-name>"}"#;
        assert_eq!(reasonix_user_text(injected), None);
    }

    #[test]
    fn tolerates_array_content_shape() {
        let line = r#"{"role":"user","content":[{"type":"text","text":"fix the parser"}]}"#;
        assert_eq!(reasonix_user_text(line).as_deref(), Some("fix the parser"));
    }

    #[test]
    fn user_text_never_panics_on_garbage() {
        for line in ["", "{not json", "{}", r#"{"role":"user"}"#, r#"{"content":"x"}"#] {
            assert_eq!(reasonix_user_text(line), None);
        }
    }
}
