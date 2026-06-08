use core::model::{ParsedEvent, Usage};
use core::store::Store;

fn ev(id: &str) -> ParsedEvent {
    ParsedEvent {
        request_id: id.into(),
        ts: 1,
        session_id: "s".into(),
        project: "p".into(),
        model: "claude-opus-4-8".into(),
        usage: Usage {
            input: 10,
            output: 5,
            ..Default::default()
        },
        source: core::model::Source::Claude,
    }
}

#[test]
fn insert_is_idempotent_on_request_id() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&ev("r1")).unwrap();
    s.insert_event(&ev("r1")).unwrap(); // dup -> ignored
    assert_eq!(s.total_tokens().unwrap(), 15); // counted once
    assert_eq!(s.event_count().unwrap(), 1);
}

#[test]
fn distinct_request_ids_accumulate() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&ev("r1")).unwrap();
    s.insert_event(&ev("r2")).unwrap();
    assert_eq!(s.event_count().unwrap(), 2);
    assert_eq!(s.total_tokens().unwrap(), 30);
}

#[test]
fn insert_batch_dedups_within_and_across_calls() {
    let s = Store::open_in_memory().unwrap();
    let batch = vec![ev("a"), ev("b"), ev("a")];
    let inserted = s.insert_batch(&batch).unwrap();
    assert_eq!(inserted, 2);
    assert_eq!(s.event_count().unwrap(), 2);
    // Re-running the same batch inserts nothing new.
    let inserted2 = s.insert_batch(&batch).unwrap();
    assert_eq!(inserted2, 0);
    assert_eq!(s.event_count().unwrap(), 2);
}

#[test]
fn offsets_roundtrip() {
    let s = Store::open_in_memory().unwrap();
    s.set_offset("f.jsonl", 123).unwrap();
    assert_eq!(s.get_offset("f.jsonl").unwrap(), 123);
}

#[test]
fn missing_offset_is_zero() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.get_offset("never-seen.jsonl").unwrap(), 0);
}

#[test]
fn offset_can_be_updated() {
    let s = Store::open_in_memory().unwrap();
    s.set_offset("f.jsonl", 10).unwrap();
    s.set_offset("f.jsonl", 250).unwrap();
    assert_eq!(s.get_offset("f.jsonl").unwrap(), 250);
}

#[test]
fn open_file_db_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    {
        let s = Store::open(&path).unwrap();
        s.insert_event(&ev("persisted")).unwrap();
        s.set_offset("x.jsonl", 99).unwrap();
    }
    // Reopen: data survives.
    let s = Store::open(&path).unwrap();
    assert_eq!(s.event_count().unwrap(), 1);
    assert_eq!(s.get_offset("x.jsonl").unwrap(), 99);
}

#[test]
fn wal_mode_is_enabled_for_file_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let s = Store::open(&path).unwrap();
    let mode = s.journal_mode().unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn store_records_source_file_and_offset_columns() {
    // insert_event_at lets the importer record provenance for incremental parse.
    let s = Store::open_in_memory().unwrap();
    s.insert_event_at(&ev("r1"), "proj/session.jsonl", 4096)
        .unwrap();
    assert_eq!(s.event_count().unwrap(), 1);
}
