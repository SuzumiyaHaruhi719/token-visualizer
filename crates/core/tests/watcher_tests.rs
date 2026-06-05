use std::sync::mpsc;
use std::time::{Duration, Instant};

use core::importer::read_new_complete_lines;
use core::store::Store;
use core::watcher::{process_file_update, watch, WatchEvent};

const ASSIST: &str = r#"{"type":"assistant","cwd":"/x/P","sessionId":"s","requestId":"r1","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":7,"output_tokens":3}}}"#;

#[test]
fn process_file_update_reads_new_and_advances_offset() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, format!("{ASSIST}\n")).unwrap();

    let store = Store::open_in_memory().unwrap();
    let (tx, rx) = mpsc::channel::<WatchEvent>();

    process_file_update(&file, &store, &tx).unwrap();

    // The store got the event, and an event was broadcast.
    assert_eq!(store.event_count().unwrap(), 1);
    let ev = rx.try_recv().expect("expected a watch event");
    match ev {
        WatchEvent::Events { events, .. } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].request_id, "r1");
        }
    }

    // Calling again with no new bytes does nothing (no double count, no event).
    process_file_update(&file, &store, &tx).unwrap();
    assert_eq!(store.event_count().unwrap(), 1);
    assert!(rx.try_recv().is_err());
}

#[test]
fn incremental_read_picks_up_only_appended_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    std::fs::write(&file, format!("{ASSIST}\n")).unwrap();
    let first = read_new_complete_lines(&file, 0).unwrap();
    assert_eq!(first.events.len(), 1);

    let second_line = ASSIST.replace("\"r1\"", "\"r2\"");
    let existing = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{existing}{second_line}\n")).unwrap();

    let next = read_new_complete_lines(&file, first.new_offset).unwrap();
    assert_eq!(next.events.len(), 1);
    assert_eq!(next.events[0].request_id, "r2");
}

#[test]
fn watch_emits_event_when_file_appended() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().to_path_buf();
    // Pre-create the file so the watcher has something to track.
    let file = projects.join("live.jsonl");
    std::fs::write(&file, "").unwrap();

    let store = Store::open_in_memory().unwrap();
    let (tx, rx) = mpsc::channel::<WatchEvent>();

    let handle = watch(&projects, store, tx).expect("watcher starts");

    // Give the watcher a moment to register, then append.
    std::thread::sleep(Duration::from_millis(300));
    let mut content = String::new();
    content.push_str(ASSIST);
    content.push('\n');
    std::fs::write(&file, &content).unwrap();

    // Expect an event within ~3s (debounce + fs latency).
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = false;
    while Instant::now() < deadline {
        if let Ok(WatchEvent::Events { events, .. }) = rx.recv_timeout(Duration::from_millis(500)) {
            if events.iter().any(|e| e.request_id == "r1") {
                got = true;
                break;
            }
        }
    }
    assert!(got, "watcher did not deliver appended event in time");

    handle.stop();
}
