use core::model::{AssistantContent, LineKind, ParsedEvent, PetState, Usage};
use core::state::{
    derive_session_state, last_user_message, parse_session_status, session_state_from,
};

fn event(id: &str) -> ParsedEvent {
    ParsedEvent {
        request_id: id.into(),
        ts: 1,
        session_id: "s".into(),
        project: "p".into(),
        model: "claude-opus-4-8".into(),
        usage: Usage {
            input: 1,
            output: 1,
            ..Default::default()
        },
        source: core::model::Source::Claude,
    }
}

/// A finalized assistant message whose content is visible text (the common
/// "responding" shape: real Claude jsonl carries usage on this line).
fn assistant(id: &str) -> LineKind {
    LineKind::Assistant {
        event: event(id),
        content: AssistantContent::Text,
    }
}

/// A finalized assistant message whose only content is a `thinking` block — the
/// real shape Claude jsonl writes for an extended-reasoning turn (usage present).
fn assistant_thinking(id: &str) -> LineKind {
    LineKind::Assistant {
        event: event(id),
        content: AssistantContent::Thinking,
    }
}

/// A finalized assistant message that issued a `tool_use` call (usage present) —
/// the shape real Claude jsonl always writes for a tool call.
fn assistant_tool(id: &str, tool_id: &str, name: &str) -> LineKind {
    LineKind::Assistant {
        event: event(id),
        content: AssistantContent::Tool {
            id: tool_id.into(),
            name: name.into(),
        },
    }
}

fn tool_use(id: &str, name: &str) -> LineKind {
    LineKind::ToolUse {
        id: id.into(),
        name: name.into(),
    }
}

fn tool_result(id: &str) -> LineKind {
    LineKind::ToolResult {
        tool_use_id: id.into(),
    }
}

fn user_text(text: &str) -> LineKind {
    LineKind::UserText(text.into())
}

// --- Line-based state derivation (staleness now lives in the poll layer) ----
// `derive_session_state(status, lines)` reads ONLY what the recent tail says; it
// no longer snaps to Idle/Sleeping by age. An active-window session therefore
// keeps its real Working/Thinking/Waiting/Responding state.

#[test]
fn busy_unmatched_tool_use_is_working_with_name() {
    let lines = vec![tool_use("tu_1", "Bash")];
    assert_eq!(
        derive_session_state("busy", &lines),
        PetState::Working(Some("Bash".into()))
    );
}

#[test]
fn busy_matched_tool_use_is_not_working() {
    // tool_use followed by its tool_result -> the call completed.
    let lines = vec![
        tool_use("tu_1", "Bash"),
        tool_result("tu_1"),
        assistant("a"),
    ];
    assert_eq!(derive_session_state("busy", &lines), PetState::Responding);
}

#[test]
fn tool_result_before_tool_use_does_not_match_future_tool() {
    let lines = vec![tool_result("tu_1"), tool_use("tu_1", "Bash")];
    assert_eq!(
        derive_session_state("busy", &lines),
        PetState::Working(Some("Bash".into()))
    );
}

#[test]
fn last_unmatched_tool_use_wins_when_multiple() {
    // First call completes; a second call (Edit) is still pending.
    let lines = vec![
        tool_use("tu_1", "Bash"),
        tool_result("tu_1"),
        tool_use("tu_2", "Edit"),
    ];
    assert_eq!(
        derive_session_state("busy", &lines),
        PetState::Working(Some("Edit".into()))
    );
}

#[test]
fn busy_last_thinking_is_thinking() {
    let lines = vec![assistant("a"), LineKind::Thinking];
    assert_eq!(derive_session_state("busy", &lines), PetState::Thinking);
}

#[test]
fn busy_end_turn_is_waiting() {
    let lines = vec![assistant("a"), LineKind::EndTurn];
    assert_eq!(derive_session_state("busy", &lines), PetState::Waiting);
}

#[test]
fn busy_producing_text_is_responding() {
    let lines = vec![assistant("a")];
    assert_eq!(derive_session_state("busy", &lines), PetState::Responding);
}

// --- The headline bug: an ACTIVE chat must never read Idle -------------------
// Previously a session that hadn't written for >60s snapped to Idle even when it
// was mid-work. Staleness is now the poll layer's job (it only derives state for
// <5min-fresh sessions), so the core derivation keeps the live state regardless
// of any notion of age — there is no longer ANY age input to this function.

#[test]
fn active_working_session_is_never_idle() {
    // A tool is still running. No matter what, this must read Working, not Idle.
    let lines = vec![assistant_tool("a", "tu_1", "Bash")];
    let st = derive_session_state("busy", &lines);
    assert_eq!(st, PetState::Working(Some("Bash".into())));
    assert_ne!(st, PetState::Idle, "an actively-working chat must never be Idle");
}

#[test]
fn active_thinking_session_is_never_idle() {
    // Mid extended-reasoning (trailing tool_result): Thinking, never Idle.
    let lines = vec![assistant_tool("a", "tu_1", "Read"), tool_result("tu_1")];
    let st = derive_session_state("busy", &lines);
    assert_eq!(st, PetState::Thinking);
    assert_ne!(st, PetState::Idle);
}

// --- Reasoning-as-activity: the priority bug ------------------------------
// Real Claude jsonl writes the reasoning of an extended-thinking turn as a
// finalized assistant message that carries `message.usage` AND whose only
// content block is `thinking`. Such a line must classify as Thinking (an ACTIVE
// state), never Idle/Responding.

#[test]
fn reasoning_assistant_message_is_thinking_not_idle() {
    let lines = vec![assistant_thinking("a")];
    let st = derive_session_state("busy", &lines);
    assert_eq!(st, PetState::Thinking);
    assert_ne!(st, PetState::Idle, "reasoning must never be Idle");
}

#[test]
fn reasoning_is_thinking_even_with_empty_status() {
    // The recent-jsonl fallback path passes an empty status string.
    let lines = vec![assistant_thinking("a")];
    assert_eq!(derive_session_state("", &lines), PetState::Thinking);
}

#[test]
fn reasoning_after_text_is_thinking() {
    let lines = vec![assistant("older"), assistant_thinking("newer")];
    assert_eq!(derive_session_state("busy", &lines), PetState::Thinking);
}

// --- Trailing tool_result is Thinking, not Responding (Q2) -----------------

#[test]
fn trailing_matched_tool_result_is_thinking() {
    let lines = vec![tool_use("tu_1", "Bash"), tool_result("tu_1")];
    let st = derive_session_state("busy", &lines);
    assert_eq!(st, PetState::Thinking);
    assert_ne!(
        st,
        PetState::Responding,
        "a trailing tool_result is reasoning, not responding"
    );
}

#[test]
fn trailing_tool_result_after_usage_tool_is_thinking() {
    let lines = vec![assistant_tool("a", "tu_1", "Read"), tool_result("tu_1")];
    assert_eq!(derive_session_state("busy", &lines), PetState::Thinking);
}

// --- Tool calls carry usage in real data ----------------------------------

#[test]
fn usage_bearing_tool_call_is_working() {
    let lines = vec![assistant_tool("a", "tu_1", "Bash")];
    assert_eq!(
        derive_session_state("busy", &lines),
        PetState::Working(Some("Bash".into()))
    );
}

#[test]
fn usage_bearing_tool_call_matched_by_result_is_not_working() {
    let lines = vec![
        assistant_tool("a", "tu_1", "Bash"),
        tool_result("tu_1"),
        assistant("b"),
    ];
    assert_eq!(derive_session_state("busy", &lines), PetState::Responding);
}

#[test]
fn last_unmatched_usage_tool_wins_over_completed_one() {
    let lines = vec![
        assistant_tool("a", "tu_1", "Read"),
        tool_result("tu_1"),
        assistant_tool("b", "tu_2", "Edit"),
    ];
    assert_eq!(
        derive_session_state("busy", &lines),
        PetState::Working(Some("Edit".into()))
    );
}

// --- A trailing user prompt is an active state ------------------------------
// The user just typed and the model hasn't started responding yet — that is an
// ACTIVE turn (about to think), not Idle.

#[test]
fn trailing_user_prompt_is_thinking() {
    let lines = vec![assistant("older"), user_text("fix the bug")];
    assert_eq!(derive_session_state("busy", &lines), PetState::Thinking);
}

// --- Coarse-status fallback is ONLY used when the tail has no signal ---------

#[test]
fn idle_status_is_fallback_only_when_no_lines() {
    // No meaningful lines + idle status -> Idle.
    let lines = vec![LineKind::Other];
    assert_eq!(derive_session_state("idle", &lines), PetState::Idle);
}

#[test]
fn idle_status_does_not_override_live_line_state() {
    // A finished turn (end_turn) must read Waiting even if the coarse status file
    // still says "idle" — the live line wins over the stale coarse status.
    let lines = vec![assistant("a"), LineKind::EndTurn];
    assert_eq!(
        derive_session_state("idle", &lines),
        PetState::Waiting,
        "live line state must win over a coarse idle status"
    );
}

#[test]
fn unknown_status_falls_back_to_lines() {
    let lines = vec![tool_use("tu_1", "Grep")];
    assert_eq!(
        derive_session_state("frobnicating", &lines),
        PetState::Working(Some("Grep".into()))
    );
}

#[test]
fn unknown_status_no_meaningful_lines_is_idle() {
    let lines = vec![LineKind::Other, LineKind::Other];
    assert_eq!(derive_session_state("frobnicate", &lines), PetState::Idle);
}

#[test]
fn empty_lines_busy_is_idle_fallback() {
    assert_eq!(derive_session_state("busy", &[]), PetState::Idle);
}

#[test]
fn tool_use_without_name_still_working_with_none() {
    let lines = vec![LineKind::ToolUse {
        id: "x".into(),
        name: String::new(),
    }];
    assert_eq!(derive_session_state("busy", &lines), PetState::Working(None));
}

// --- last_user_message extraction from lines --------------------------------

#[test]
fn last_user_message_picks_most_recent_prompt() {
    let lines = vec![
        user_text("first prompt"),
        assistant("a"),
        user_text("second prompt"),
        assistant("b"),
    ];
    assert_eq!(last_user_message(&lines), "second prompt");
}

#[test]
fn last_user_message_empty_when_none() {
    let lines = vec![assistant("a"), tool_result("tu_1")];
    assert_eq!(last_user_message(&lines), "");
}

#[test]
fn last_user_message_ignores_tool_results() {
    // Only UserText counts; a trailing tool_result is not a user prompt.
    let lines = vec![user_text("the real prompt"), tool_result("tu_1")];
    assert_eq!(last_user_message(&lines), "the real prompt");
}

// --- The packing helper sets updated_at = last_activity + last_user_message --

#[test]
fn session_state_helper_packs_fields() {
    const LAST_ACTIVITY: i64 = 1_700_000_000_123;
    let lines = vec![user_text("do the thing"), tool_use("tu_1", "Bash")];
    let ss = session_state_from(
        "sess-42",
        "CorePilot",
        "claude-opus-4-8",
        "busy",
        &lines,
        1234,
        LAST_ACTIVITY,
        core::model::Source::Claude,
    );
    assert_eq!(ss.session_id, "sess-42");
    assert_eq!(ss.project, "CorePilot");
    assert_eq!(ss.model, "claude-opus-4-8");
    assert_eq!(ss.tokens, 1234);
    // updated_at is the LAST ACTIVITY timestamp, not the poll time.
    assert_eq!(ss.updated_at, LAST_ACTIVITY);
    assert_eq!(ss.last_user_message, "do the thing");
    assert_eq!(ss.state, PetState::Working(Some("Bash".into())));
    assert_eq!(ss.source, core::model::Source::Claude);
}

#[test]
fn parse_session_status_reads_fields() {
    let json = r#"{"status":"busy","updatedAt":"2026-06-05T10:00:00.000Z","sessionId":"abc"}"#;
    let s = parse_session_status(json);
    assert_eq!(s.status, "busy");
    assert_eq!(s.updated_at, 1_780_653_600_000);
    assert_eq!(s.session_id.as_deref(), Some("abc"));
}

#[test]
fn parse_session_status_accepts_epoch_millis() {
    let json = r#"{"status":"idle","updatedAt":1780653600000}"#;
    let s = parse_session_status(json);
    assert_eq!(s.status, "idle");
    assert_eq!(s.updated_at, 1_780_653_600_000);
}

#[test]
fn parse_session_status_tolerates_garbage() {
    let s = parse_session_status("{not json");
    assert_eq!(s.status, "");
    assert_eq!(s.updated_at, 0);
    assert!(s.session_id.is_none());
}
