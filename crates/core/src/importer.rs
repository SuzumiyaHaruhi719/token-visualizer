//! Backfill: walk `*.jsonl` recursively and stream new bytes into the store.
//!
//! Streaming + offset tracking means re-running is cheap and idempotent: each
//! file is read only from its stored byte offset, and only COMPLETE lines (those
//! terminated by `\n`) are consumed. A half-written trailing line does not
//! advance the offset, so it is re-read once completed (design §8).

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

use crate::codex::{codex_event, parse_codex_line, CodexLine};
use crate::model::{LineKind, ParsedEvent};
use crate::parser::parse_line;
use crate::paths::project_name_from_cwd;
use crate::store::Store;

/// Result of reading new complete lines from a file.
pub struct IncrementalRead {
    /// Byte offset where this read actually started. This differs from the
    /// requested offset when a file has shrunk and must be re-read from zero.
    pub start_offset: u64,
    /// Assistant events parsed from the newly-read complete lines.
    pub events: Vec<ParsedEvent>,
    /// All line kinds parsed (for live state derivation by the watcher).
    pub lines: Vec<LineKind>,
    /// Byte offset after the last COMPLETE line; persist this.
    pub new_offset: u64,
}

/// Walk `projects_dir` for `*.jsonl` files and import any new complete lines.
///
/// `progress(done, total)` is called after each file is processed.
pub fn backfill(
    projects_dir: &Path,
    store: &Store,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    let files = collect_jsonl(projects_dir);
    let total = files.len();

    for (idx, path) in files.iter().enumerate() {
        let key = path.to_string_lossy().to_string();
        let offset = store.get_offset(&key)?;
        let read = read_new_complete_lines(path, offset)?;

        if !read.events.is_empty() {
            // Record provenance (source_file, line_offset == start offset).
            let start_offset = read.start_offset as i64;
            let batch: Vec<(ParsedEvent, i64)> =
                read.events.into_iter().map(|e| (e, start_offset)).collect();
            store.insert_batch_at(&batch, &key)?;
        }
        store.set_offset(&key, read.new_offset)?;

        progress(idx + 1, total);
    }
    Ok(())
}

/// Walk `codex_sessions_dir` for `rollout-*.jsonl` files and import any new
/// complete `token_count` turns. Mirrors [`backfill`] but parses the Codex
/// rollout shape (read-only; offset-tracked; idempotent).
///
/// Each rollout file's offset is namespaced (`codex:<path>`) so it never
/// collides with a Claude file key in `import_state`.
pub fn backfill_codex(
    codex_sessions_dir: &Path,
    store: &Store,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    let files = collect_codex_rollouts(codex_sessions_dir);
    let total = files.len();

    for (idx, path) in files.iter().enumerate() {
        let key = codex_offset_key(path);
        let offset = store.get_offset(&key)?;
        let read = read_new_codex_events(path, offset)?;

        if !read.events.is_empty() {
            let start = read.start_offset as i64;
            let batch: Vec<(ParsedEvent, i64)> =
                read.events.into_iter().map(|e| (e, start)).collect();
            store.insert_batch_at(&batch, &path.to_string_lossy())?;
        }
        store.set_offset(&key, read.new_offset)?;

        progress(idx + 1, total);
    }
    Ok(())
}

/// `import_state` key for a Codex rollout file (namespaced to avoid colliding
/// with Claude file keys).
pub fn codex_offset_key(path: &Path) -> String {
    format!("codex:{}", path.to_string_lossy())
}

/// Recursively collect `rollout-*.jsonl` files under `dir`, sorted.
fn collect_codex_rollouts(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension().map(|x| x == "jsonl").unwrap_or(false)
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("rollout-"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// Extract the session uuid from a `rollout-<ISO>-<uuid>.jsonl` filename.
/// The uuid is the trailing five dash-separated groups. Falls back to the full
/// stem if the shape is unexpected.
fn session_id_from_rollout(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() >= 5 {
        parts[parts.len() - 5..].join("-")
    } else {
        stem.to_string()
    }
}

/// Read new complete Codex lines from `offset`, emitting one [`ParsedEvent`] per
/// `token_count` turn. The per-event byte offset (start of its line) is the
/// dedup key, so re-reads collapse to the same row.
///
/// Tracks the most recent `model` / `cwd`-derived project / session id seen so
/// far in the stream; the session id falls back to the filename uuid.
pub fn read_new_codex_events(path: &Path, offset: u64) -> Result<IncrementalRead> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = if offset > len { 0 } else { offset };
    let file_session_id = session_id_from_rollout(path);

    if start == len {
        return Ok(IncrementalRead {
            start_offset: start,
            events: Vec::new(),
            lines: Vec::new(),
            new_offset: start,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);

    let mut events = Vec::new();
    let mut consumed = start;
    let mut buf = Vec::new();

    // Stream state carried forward across lines.
    let mut model = String::new();
    let mut session_id = file_session_id.clone();
    let mut project = "unknown".to_string();

    loop {
        let line_offset = consumed;
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF
        }
        if buf.last() != Some(&b'\n') {
            break; // half-written trailing line: do not advance past it
        }
        consumed += n as u64;
        let text = String::from_utf8_lossy(&buf);

        // Opportunistically capture model / cwd / session id from any line.
        if let Some(cwd) = serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .as_ref()
            .and_then(|v| v.get("payload"))
            .and_then(|p| p.get("cwd"))
            .and_then(|c| c.as_str())
        {
            project = project_name_from_cwd(cwd);
        }

        match parse_codex_line(&text) {
            CodexLine::Model(m) => model = m,
            CodexLine::SessionMeta { id } => session_id = id,
            CodexLine::TokenCount { last, .. } => {
                // Skip zero-usage turns (e.g. an info:null heartbeat).
                if last.total > 0 || last.input > 0 || last.output > 0 {
                    let ts = parse_ts_millis(&text);
                    events.push(codex_event(
                        &last,
                        &session_id,
                        &model,
                        &project,
                        ts,
                        line_offset as i64,
                    ));
                }
            }
            CodexLine::Other => {}
        }
    }

    Ok(IncrementalRead {
        start_offset: start,
        events,
        lines: Vec::new(),
        new_offset: consumed,
    })
}

/// Best-effort epoch-millis from a Codex line's top-level `timestamp`.
fn parse_ts_millis(line: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .as_ref()
        .and_then(|v| v.get("timestamp"))
        .and_then(|t| t.as_str())
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.timestamp_millis())
        })
        .unwrap_or(0)
}

/// Recursively collect `*.jsonl` files under `dir`, sorted for determinism.
fn collect_jsonl(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Read from `offset` to EOF, returning parsed events + the new offset (end of
/// the last complete, newline-terminated line). Streams line-by-line; never
/// loads the whole file.
pub fn read_new_complete_lines(path: &Path, offset: u64) -> Result<IncrementalRead> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = if offset > len { 0 } else { offset };
    if start == len {
        return Ok(IncrementalRead {
            start_offset: start,
            events: Vec::new(),
            lines: Vec::new(),
            new_offset: start,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);

    let mut events = Vec::new();
    let mut lines = Vec::new();
    let mut consumed = start;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF
        }
        // Only consume the line if it is newline-terminated (complete).
        if buf.last() != Some(&b'\n') {
            // Half-written trailing line: stop WITHOUT advancing past it.
            break;
        }
        consumed += n as u64;
        let text = String::from_utf8_lossy(&buf);
        let kind = parse_line(&text);
        if let LineKind::Assistant(ref e) = kind {
            events.push(e.clone());
        }
        lines.push(kind);
    }

    Ok(IncrementalRead {
        start_offset: start,
        events,
        lines,
        new_offset: consumed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    /// A real-shape Codex rollout (session_meta + turn_context + token_count)
    /// round-trips through the incremental reader into one Codex event.
    #[test]
    fn reads_codex_rollout_fixture() {
        let dir = std::env::temp_dir().join(format!("cm-codex-fix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-03-21T01-04-26-019d0ec7-c4ce-71e0-b486-88e6f33baa31.jsonl");
        let lines = [
            r#"{"timestamp":"2026-03-21T05:04:40.977Z","type":"session_meta","payload":{"id":"019d0ec7-c4ce-71e0-b486-88e6f33baa31","cwd":"C:\\Users\\Thomas\\Documents\\New project"}}"#,
            r#"{"timestamp":"2026-03-21T05:04:40.979Z","type":"turn_context","payload":{"cwd":"C:\\Users\\Thomas\\Documents\\New project","model":"gpt-5.4"}}"#,
            r#"{"timestamp":"2026-03-21T05:12:04.528Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":184499,"cached_input_tokens":142336,"output_tokens":2547,"reasoning_output_tokens":1074,"total_tokens":187046},"last_token_usage":{"input_tokens":27477,"cached_input_tokens":27136,"output_tokens":124,"reasoning_output_tokens":21,"total_tokens":27601}},"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1},"secondary":{"used_percent":0.0,"window_minutes":10080,"resets_at":2},"plan_type":"plus"}}}"#,
        ];
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let read = read_new_codex_events(&path, 0).unwrap();
        assert_eq!(read.events.len(), 1);
        let e = &read.events[0];
        assert_eq!(e.source, crate::model::Source::Codex);
        assert_eq!(e.model, "gpt-5.4");
        assert_eq!(e.session_id, "019d0ec7-c4ce-71e0-b486-88e6f33baa31");
        assert_eq!(e.project, "New project");
        assert_eq!(e.usage.cache_read, 27136);
        assert_eq!(e.usage.output, 124 + 21);
        assert!(e.request_id.starts_with("codex:019d0ec7-c4ce-71e0-b486-88e6f33baa31:"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backfill is idempotent: a second pass inserts no new rows.
    #[test]
    fn codex_backfill_idempotent() {
        let dir = std::env::temp_dir().join(format!("cm-codex-bf-{}", std::process::id()));
        let sessions = dir.join("2026").join("03").join("21");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout-2026-03-21T01-04-26-aaaa-bbbb-cccc-dddd-eeee.jsonl");
        let line = r#"{"timestamp":"2026-03-21T05:12:04.528Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10,"reasoning_output_tokens":5,"total_tokens":115}}}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let store = Store::open_in_memory().unwrap();
        backfill_codex(&dir, &store, |_, _| {}).unwrap();
        let first = store.event_count().unwrap();
        backfill_codex(&dir, &store, |_, _| {}).unwrap();
        let second = store.event_count().unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backfill against the real `~/.codex/sessions` if present; skip otherwise.
    #[test]
    fn codex_backfill_real_dir_if_present() {
        let dir = match crate::paths::codex_sessions_dir() {
            Ok(d) if d.is_dir() => d,
            _ => return, // no Codex data on this machine: skip
        };
        let store = Store::open_in_memory().unwrap();
        // Must not panic and must be idempotent on the real corpus.
        backfill_codex(&dir, &store, |_, _| {}).unwrap();
        let first = store.event_count().unwrap();
        backfill_codex(&dir, &store, |_, _| {}).unwrap();
        assert_eq!(first, store.event_count().unwrap());
    }
}
