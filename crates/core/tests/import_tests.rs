use std::path::{Path, PathBuf};

use core::importer::backfill;
use core::store::Store;

fn fixtures_projects() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("projects")
}

#[test]
fn backfill_imports_all_assistant_events() {
    let store = Store::open_in_memory().unwrap();
    let mut last = (0usize, 0usize);
    backfill(&fixtures_projects(), &store, |done, total| {
        last = (done, total)
    })
    .unwrap();

    // 3 assistant events total across the two fixture files (corrupt + non-usage
    // lines skipped).
    assert_eq!(store.event_count().unwrap(), 3);
    // tokens: 110 + 420 + 560 = 1090
    assert_eq!(store.total_tokens().unwrap(), 1090);
    // progress reported all files processed.
    assert_eq!(last.0, last.1);
    assert!(last.1 >= 2, "should have walked at least 2 files");
}

#[test]
fn backfill_is_idempotent() {
    let store = Store::open_in_memory().unwrap();
    backfill(&fixtures_projects(), &store, |_, _| {}).unwrap();
    let after_first = store.total_tokens().unwrap();
    let count_first = store.event_count().unwrap();

    // Running again must not double count.
    backfill(&fixtures_projects(), &store, |_, _| {}).unwrap();
    assert_eq!(store.total_tokens().unwrap(), after_first);
    assert_eq!(store.event_count().unwrap(), count_first);
}

#[test]
fn backfill_resumes_from_offset_on_append() {
    // Copy fixtures into a temp dir so we can append without touching the repo.
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("projects").join("p");
    std::fs::create_dir_all(&proj).unwrap();
    let file = proj.join("s.jsonl");
    let line_a = r#"{"type":"assistant","cwd":"/x/P","sessionId":"s","requestId":"r1","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":0}}}"#;
    std::fs::write(&file, format!("{line_a}\n")).unwrap();

    let store = Store::open_in_memory().unwrap();
    backfill(&dir.path().join("projects"), &store, |_, _| {}).unwrap();
    assert_eq!(store.event_count().unwrap(), 1);
    assert_eq!(store.total_tokens().unwrap(), 100);

    // Append a new event and re-run: only the new event is read.
    let line_b = r#"{"type":"assistant","cwd":"/x/P","sessionId":"s","requestId":"r2","timestamp":"2026-06-05T10:01:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":50,"output_tokens":0}}}"#;
    let existing = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{existing}{line_b}\n")).unwrap();

    backfill(&dir.path().join("projects"), &store, |_, _| {}).unwrap();
    assert_eq!(store.event_count().unwrap(), 2);
    assert_eq!(store.total_tokens().unwrap(), 150);
}

#[test]
fn backfill_does_not_advance_offset_past_half_written_line() {
    // A trailing line with no newline is "half-written"; the importer must not
    // consume it, so a later completion is still picked up.
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("projects").join("p");
    std::fs::create_dir_all(&proj).unwrap();
    let file = proj.join("s.jsonl");

    let complete = r#"{"type":"assistant","cwd":"/x/P","sessionId":"s","requestId":"r1","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":0}}}"#;
    // No trailing newline on the second (partial) line.
    let partial = r#"{"type":"assistant","sessionId":"s","requestId":"r2","timestamp":"2026-06-05T10:01:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_t"#;
    std::fs::write(&file, format!("{complete}\n{partial}")).unwrap();

    let store = Store::open_in_memory().unwrap();
    backfill(&dir.path().join("projects"), &store, |_, _| {}).unwrap();
    // Only the complete line counted.
    assert_eq!(store.event_count().unwrap(), 1);

    // Now complete the partial line (overwrite with both full lines + newline).
    let full_r2 = r#"{"type":"assistant","cwd":"/x/P","sessionId":"s","requestId":"r2","timestamp":"2026-06-05T10:01:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":50,"output_tokens":0}}}"#;
    std::fs::write(&file, format!("{complete}\n{full_r2}\n")).unwrap();

    backfill(&dir.path().join("projects"), &store, |_, _| {}).unwrap();
    assert_eq!(store.event_count().unwrap(), 2);
    assert_eq!(store.total_tokens().unwrap(), 150);
}

#[test]
fn backfill_empty_dir_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    backfill(dir.path(), &store, |_, _| {}).unwrap();
    assert_eq!(store.event_count().unwrap(), 0);
}
