use core::model::{LineKind, ParsedEvent, PetState, Usage};
use core::state::{
    derive_session_state, parse_session_status, session_state_from, IDLE_MS, SLEEP_MS,
};

fn assistant(id: &str) -> LineKind {
    LineKind::Assistant(ParsedEvent {
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
    })
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

// now == last_activity so staleness never triggers in these.
const NOW: i64 = 1_000_000;

#[test]
fn busy_unmatched_tool_use_is_working_with_name() {
    let lines = vec![tool_use("tu_1", "Bash")];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Working(Some("Bash".into())));
}

#[test]
fn busy_matched_tool_use_is_not_working() {
    // tool_use followed by its tool_result -> the call completed.
    let lines = vec![
        tool_use("tu_1", "Bash"),
        tool_result("tu_1"),
        assistant("a"),
    ];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Responding);
}

#[test]
fn tool_result_before_tool_use_does_not_match_future_tool() {
    let lines = vec![tool_result("tu_1"), tool_use("tu_1", "Bash")];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Working(Some("Bash".into())));
}

#[test]
fn last_unmatched_tool_use_wins_when_multiple() {
    // First call completes; a second call (Edit) is still pending.
    let lines = vec![
        tool_use("tu_1", "Bash"),
        tool_result("tu_1"),
        tool_use("tu_2", "Edit"),
    ];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Working(Some("Edit".into())));
}

#[test]
fn busy_last_thinking_is_thinking() {
    let lines = vec![assistant("a"), LineKind::Thinking];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Thinking);
}

#[test]
fn busy_end_turn_is_waiting() {
    let lines = vec![assistant("a"), LineKind::EndTurn];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Waiting);
}

#[test]
fn busy_producing_text_is_responding() {
    let lines = vec![assistant("a")];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Responding);
}

#[test]
fn idle_status_is_idle() {
    let lines = vec![assistant("a")];
    let st = derive_session_state("idle", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Idle);
}

#[test]
fn stale_heartbeat_is_idle_even_if_busy() {
    let lines = vec![tool_use("tu_1", "Bash")];
    let last = NOW - (IDLE_MS + 1);
    let st = derive_session_state("busy", &lines, 0, NOW, last);
    assert_eq!(st, PetState::Idle);
}

#[test]
fn very_stale_heartbeat_is_sleeping() {
    let lines = vec![tool_use("tu_1", "Bash")];
    let last = NOW - (SLEEP_MS + 1);
    let st = derive_session_state("busy", &lines, 0, NOW, last);
    assert_eq!(st, PetState::Sleeping);
}

#[test]
fn just_under_idle_threshold_still_derives_from_lines() {
    let lines = vec![tool_use("tu_1", "Read")];
    let last = NOW - (IDLE_MS - 1);
    let st = derive_session_state("busy", &lines, 0, NOW, last);
    assert_eq!(st, PetState::Working(Some("Read".into())));
}

#[test]
fn unknown_status_falls_back_to_lines() {
    let lines = vec![tool_use("tu_1", "Grep")];
    let st = derive_session_state("frobnicating", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Working(Some("Grep".into())));
}

#[test]
fn unknown_status_no_meaningful_lines_is_idle() {
    let lines = vec![LineKind::Other, LineKind::Other];
    let st = derive_session_state("frobnicate", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Idle);
}

#[test]
fn empty_lines_busy_is_idle_fallback() {
    let st = derive_session_state("busy", &[], 0, NOW, NOW);
    assert_eq!(st, PetState::Idle);
}

#[test]
fn tool_use_without_name_still_working_with_none() {
    let lines = vec![LineKind::ToolUse {
        id: "x".into(),
        name: String::new(),
    }];
    let st = derive_session_state("busy", &lines, 0, NOW, NOW);
    assert_eq!(st, PetState::Working(None));
}

#[test]
fn session_state_helper_packs_fields() {
    let lines = vec![tool_use("tu_1", "Bash")];
    let ss = session_state_from(
        "sess-42",
        "CorePilot",
        "claude-opus-4-8",
        "busy",
        &lines,
        1234,
        NOW,
        NOW,
    );
    assert_eq!(ss.session_id, "sess-42");
    assert_eq!(ss.project, "CorePilot");
    assert_eq!(ss.model, "claude-opus-4-8");
    assert_eq!(ss.tokens, 1234);
    assert_eq!(ss.updated_at, NOW);
    assert_eq!(ss.state, PetState::Working(Some("Bash".into())));
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
