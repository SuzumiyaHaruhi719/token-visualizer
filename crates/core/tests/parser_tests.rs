use core::model::LineKind;
use core::parser::parse_line;

const ASSISTANT: &str = r#"{"type":"assistant","cwd":"C:\\Users\\Thomas\\Documents\\Projects\\CorePilot","sessionId":"abc","requestId":"req_1","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","stop_reason":"tool_use","content":[{"type":"tool_use","id":"tu_9","name":"Bash"}],"usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":50,"cache_read_input_tokens":2000}}}"#;

#[test]
fn parses_assistant_usage() {
    match parse_line(ASSISTANT) {
        LineKind::Assistant(e) => {
            assert_eq!(e.request_id, "req_1");
            assert_eq!(e.model, "claude-opus-4-8");
            assert_eq!(e.project, "CorePilot");
            assert_eq!(e.session_id, "abc");
            assert_eq!(e.usage.input, 100);
            assert_eq!(e.usage.output, 20);
            assert_eq!(e.usage.cache_create, 50);
            assert_eq!(e.usage.cache_read, 2000);
            assert_eq!(e.usage.total(), 2170);
            // 2026-06-05T10:00:00.000Z = 1780653600000 ms
            assert_eq!(e.ts, 1_780_653_600_000);
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[test]
fn assistant_falls_back_to_uuid_when_no_request_id() {
    let line = r#"{"type":"assistant","cwd":"/home/u/Proj","sessionId":"s","uuid":"uuid-xyz","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
    match parse_line(line) {
        LineKind::Assistant(e) => assert_eq!(e.request_id, "uuid-xyz"),
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[test]
fn assistant_captures_server_tool_use_counts() {
    let line = r#"{"type":"assistant","cwd":"/p/Proj","sessionId":"s","requestId":"r","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1,"output_tokens":1,"server_tool_use":{"web_search_requests":3,"web_fetch_requests":2}}}}"#;
    match parse_line(line) {
        LineKind::Assistant(e) => {
            assert_eq!(e.usage.web_search, 3);
            assert_eq!(e.usage.web_fetch, 2);
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[test]
fn assistant_falls_back_to_jsonl_dir_when_cwd_missing() {
    // No cwd field at all -> project resolves to "unknown" (parse_line has no
    // file context); the importer supplies a directory-based fallback instead.
    let line = r#"{"type":"assistant","sessionId":"s","requestId":"r","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1,"output_tokens":1}}}"#;
    match parse_line(line) {
        LineKind::Assistant(e) => assert_eq!(e.project, "unknown"),
        other => panic!("expected Assistant, got {other:?}"),
    }
}

#[test]
fn detects_tool_use() {
    // assistant content with only a tool_use block and NO usage -> ToolUse
    let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_42","name":"Bash"}]}}"#;
    match parse_line(line) {
        LineKind::ToolUse { id, name } => {
            assert_eq!(id, "tu_42");
            assert_eq!(name, "Bash");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

#[test]
fn detects_tool_result() {
    let line =
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_9"}]}}"#;
    match parse_line(line) {
        LineKind::ToolResult { tool_use_id } => assert_eq!(tool_use_id, "tu_9"),
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn detects_thinking() {
    let line =
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
    assert_eq!(parse_line(line), LineKind::Thinking);
}

#[test]
fn end_turn_detected() {
    // stop_reason end_turn, content is text only (no tool_use), no usage
    let line = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#;
    assert_eq!(parse_line(line), LineKind::EndTurn);
}

#[test]
fn tool_use_takes_priority_over_end_turn() {
    // If a line has both a tool_use block and stop_reason end_turn, tool_use wins.
    let line = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"tool_use","id":"t","name":"Read"}]}}"#;
    match parse_line(line) {
        LineKind::ToolUse { name, .. } => assert_eq!(name, "Read"),
        other => panic!("expected ToolUse priority, got {other:?}"),
    }
}

#[test]
fn skips_non_usage_lines() {
    assert_eq!(parse_line(r#"{"type":"summary"}"#), LineKind::Other);
    assert_eq!(
        parse_line(r#"{"type":"user","message":{"content":"hello"}}"#),
        LineKind::Other
    );
    assert_eq!(parse_line(r#"{"type":"system"}"#), LineKind::Other);
}

#[test]
fn corrupt_line_is_other_not_panic() {
    assert_eq!(parse_line("{not json"), LineKind::Other);
    assert_eq!(parse_line(""), LineKind::Other);
    assert_eq!(parse_line("   "), LineKind::Other);
    assert_eq!(parse_line("[]"), LineKind::Other);
    assert_eq!(parse_line("42"), LineKind::Other);
}

#[test]
fn half_written_line_is_other() {
    assert_eq!(parse_line(r#"{"type":"assi"#), LineKind::Other);
    assert_eq!(
        parse_line(r#"{"type":"assistant","message":{"usage":{"input_tokens":1"#),
        LineKind::Other
    );
}

#[test]
fn malformed_and_odd_shapes_never_panic() {
    let cases = [
        "{",
        "[",
        r#"{"message":null}"#,
        r#"{"message":{"usage":[]}}"#,
        r#"{"message":{"usage":{"input_tokens":{}}}}"#,
        r#"{"message":{"content":{}}}"#,
        r#"{"message":{"content":[null,42,"text",{}]}}"#,
        r#"{"timestamp":false,"message":{"usage":{"input_tokens":"nope"}}}"#,
        r#"{"message":{"content":[{"type":"tool_use","id":42,"name":false}]}}"#,
    ];

    for line in cases {
        let parsed = std::panic::catch_unwind(|| parse_line(line));
        assert!(parsed.is_ok(), "parse_line panicked for {line}");
    }
}

#[test]
fn assistant_with_missing_usage_fields_defaults_to_zero() {
    let line = r#"{"type":"assistant","cwd":"/p/X","sessionId":"s","requestId":"r","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":7}}}"#;
    match parse_line(line) {
        LineKind::Assistant(e) => {
            assert_eq!(e.usage.input, 7);
            assert_eq!(e.usage.output, 0);
            assert_eq!(e.usage.cache_create, 0);
            assert_eq!(e.usage.cache_read, 0);
        }
        other => panic!("expected Assistant, got {other:?}"),
    }
}
