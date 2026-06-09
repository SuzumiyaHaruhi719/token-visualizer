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
use crate::reasonix::{parse_usage_line, reasonix_event};
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

/// `import_state` key for the Reasonix `usage.jsonl` file (namespaced so it never
/// collides with a Claude or Codex file key).
pub fn reasonix_offset_key(path: &Path) -> String {
    format!("reasonix:{}", path.to_string_lossy())
}

/// Backfill the Reasonix `usage.jsonl` (the token + cost source). Mirrors
/// [`backfill_codex`] but reads the flat one-turn-per-line shape (read-only;
/// offset-tracked; idempotent). `sessions_dir` is used to resolve each turn's
/// project name from its session's `<name>.meta.json` workspace.
pub fn backfill_reasonix(
    usage_path: &Path,
    sessions_dir: &Path,
    store: &Store,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    if !usage_path.is_file() {
        progress(0, 0);
        return Ok(());
    }
    let key = reasonix_offset_key(usage_path);
    let offset = store.get_offset(&key)?;
    let read = read_new_reasonix_events(usage_path, sessions_dir, offset)?;

    if !read.events.is_empty() {
        let start = read.start_offset as i64;
        let batch: Vec<(ParsedEvent, i64)> =
            read.events.into_iter().map(|e| (e, start)).collect();
        store.insert_batch_at(&batch, &usage_path.to_string_lossy())?;
    }
    store.set_offset(&key, read.new_offset)?;
    progress(1, 1);
    Ok(())
}

/// Read new complete Reasonix `usage.jsonl` lines from `offset`, emitting one
/// [`ParsedEvent`] per billable turn. Each line is self-contained (it carries its
/// own `ts`, `session`, `model`, and token counts), so — unlike Codex — there is
/// NO head replay for carried context. The per-line byte offset is the dedup key
/// (`reasonix:<session>:<offset>`), so re-reads collapse to the same row.
///
/// `sessions_dir` resolves each turn's project name from its
/// `<session>.meta.json` workspace; an unresolved project falls back to the
/// session name. Tolerant: malformed / zero-token lines are skipped, never fatal.
pub fn read_new_reasonix_events(
    usage_path: &Path,
    sessions_dir: &Path,
    offset: u64,
) -> Result<IncrementalRead> {
    let mut file = File::open(usage_path)?;
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
    let mut consumed = start;
    let mut buf = Vec::new();
    // Cache `session -> project` so we read each `<session>.meta.json` at most
    // once per backfill pass (a session appears on many consecutive lines).
    let mut project_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

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

        if let Some(line) = parse_usage_line(&text) {
            // Skip empty heartbeat lines (no billable tokens).
            if line.total_tokens() > 0 {
                let project = project_cache
                    .entry(line.session.clone())
                    .or_insert_with(|| reasonix_project_for_session(sessions_dir, &line.session))
                    .clone();
                events.push(reasonix_event(&line, &project, line_offset as i64));
            }
        }
    }

    Ok(IncrementalRead {
        start_offset: start,
        events,
        lines: Vec::new(),
        new_offset: consumed,
    })
}

/// Resolve a Reasonix session's friendly project name from its
/// `sessions/<session>.meta.json` `workspace` (its basename), falling back to the
/// session name itself when the meta file is absent or carries no workspace.
pub fn reasonix_project_for_session(sessions_dir: &Path, session: &str) -> String {
    let meta = sessions_dir.join(format!("{session}.meta.json"));
    let workspace = std::fs::read_to_string(&meta)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("workspace")
                .and_then(|w| w.as_str())
                .map(str::to_string)
        });
    match workspace {
        Some(ws) if !ws.trim().is_empty() => project_name_from_cwd(&ws),
        _ => session.to_string(),
    }
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
/// far in the stream; the session id falls back to the filename uuid. Codex
/// declares the model once near the top of a rollout and then appends many
/// `token_count` turns, so before emitting we replay `[0, offset)` for context
/// only — otherwise a tail read beginning past the last `turn_context` would
/// lose the model and bucket every live turn under "" (shown as "other").
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
    // Stream state carried forward across lines.
    let mut model = String::new();
    let mut session_id = file_session_id.clone();
    let mut project = "unknown".to_string();

    // Replay the head of the file [0, start) for context only — rebuilds the
    // model / project / session id in effect at `start` without re-emitting the
    // turns that were already read on an earlier pass.
    if start > 0 {
        file.seek(SeekFrom::Start(0))?;
        let mut head = BufReader::new(&mut file);
        let mut scanned = 0u64;
        let mut hbuf = Vec::new();
        while scanned < start {
            hbuf.clear();
            let n = head.read_until(b'\n', &mut hbuf)?;
            if n == 0 {
                break;
            }
            scanned += n as u64;
            let text = String::from_utf8_lossy(&hbuf);
            apply_codex_context(&text, &mut model, &mut session_id, &mut project);
        }
    }

    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);

    let mut events = Vec::new();
    let mut consumed = start;
    let mut buf = Vec::new();

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

        // Update carried context (model / project / session) and, if this line
        // is a billable turn, emit it attributed to the current model.
        if let CodexLine::TokenCount { last, .. } =
            apply_codex_context(&text, &mut model, &mut session_id, &mut project)
        {
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
    }

    Ok(IncrementalRead {
        start_offset: start,
        events,
        lines: Vec::new(),
        new_offset: consumed,
    })
}

/// Update the carried-forward `model` / `session_id` / `project` stream state
/// from one Codex rollout line, returning the parsed [`CodexLine`] so callers
/// can also act on `token_count` turns without re-parsing.
fn apply_codex_context(
    text: &str,
    model: &mut String,
    session_id: &mut String,
    project: &mut String,
) -> CodexLine {
    // Opportunistically capture the cwd-derived project from any line.
    if let Some(cwd) = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .as_ref()
        .and_then(|v| v.get("payload"))
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
    {
        *project = project_name_from_cwd(cwd);
    }
    let parsed = parse_codex_line(text);
    match &parsed {
        CodexLine::Model(m) => *model = m.clone(),
        CodexLine::SessionMeta { id } => *session_id = id.clone(),
        _ => {}
    }
    parsed
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
        if let Some(e) = kind.event() {
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

    /// Regression: an incremental tail read (offset past the `turn_context`
    /// line) must still attribute the live `token_count` turns to the session's
    /// model. Codex declares the model once near the top, then appends many
    /// `token_count` events; a watcher tailing from a stored offset would lose
    /// the model and bucket every live turn under "" (shown as "other").
    #[test]
    fn incremental_read_keeps_model_after_offset() {
        let dir = std::env::temp_dir().join(format!("cm-codex-inc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-06-08T17-06-23-019ea67b-aad4-7130-bd8f-1c350901c15f.jsonl");

        let session_meta = r#"{"timestamp":"2026-06-08T09:06:24.219Z","type":"session_meta","payload":{"id":"019ea67b-aad4-7130-bd8f-1c350901c15f","cwd":"C:\\Users\\Thomas\\Documents\\WT-GCI-OSD"}}"#;
        let turn_context = r#"{"timestamp":"2026-06-08T09:06:25.000Z","type":"turn_context","payload":{"cwd":"C:\\Users\\Thomas\\Documents\\WT-GCI-OSD","model":"gpt-5.5"}}"#;
        let tc1 = r#"{"timestamp":"2026-06-08T09:10:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":50,"reasoning_output_tokens":10,"total_tokens":1060}}}}"#;
        let tc2 = r#"{"timestamp":"2026-06-08T09:20:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":2000,"cached_input_tokens":800,"output_tokens":70,"reasoning_output_tokens":20,"total_tokens":2090}}}}"#;

        // First pass: full read from 0 captures the model from turn_context.
        std::fs::write(
            &path,
            format!("{session_meta}\n{turn_context}\n{tc1}\n"),
        )
        .unwrap();
        let first = read_new_codex_events(&path, 0).unwrap();
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].model, "gpt-5.5");

        // Codex appends another turn — but NO fresh turn_context line.
        let mut full = std::fs::read_to_string(&path).unwrap();
        full.push_str(tc2);
        full.push('\n');
        std::fs::write(&path, &full).unwrap();

        // Tail read from the stored offset: the new turn must STILL be gpt-5.5,
        // not "" (which the query layer relabels to "other").
        let tail = read_new_codex_events(&path, first.new_offset).unwrap();
        assert_eq!(tail.events.len(), 1, "exactly the one appended turn");
        assert_eq!(
            tail.events[0].model, "gpt-5.5",
            "tailed turn must inherit the session model, not fall back to \"\""
        );
        assert_eq!(tail.events[0].session_id, "019ea67b-aad4-7130-bd8f-1c350901c15f");
        assert_eq!(tail.events[0].project, "WT-GCI-OSD");

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

    // --- Reasonix (DeepSeek) importer ---------------------------------------

    fn reasonix_tmp() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cm-reasonix-imp-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two real-shape usage.jsonl turns import as two DeepSeek events with the
    /// right token mapping + the project resolved from the session meta.
    #[test]
    fn reads_reasonix_usage_fixture() {
        let dir = reasonix_tmp();
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // meta gives the workspace -> project basename "Projects".
        std::fs::write(
            sessions.join("code-Projects.meta.json"),
            r#"{"summary":"deploy","workspace":"C:\\Users\\Thomas\\Documents\\Projects"}"#,
        )
        .unwrap();
        let usage = dir.join("usage.jsonl");
        let lines = [
            r#"{"ts":1780916586293,"session":"code-Projects","model":"deepseek-v4-flash","promptTokens":23015,"completionTokens":140,"cacheHitTokens":0,"cacheMissTokens":23015,"costUsd":0.0032613}"#,
            r#"{"ts":1780970065293,"session":"code-Projects","model":"deepseek-v4-pro","promptTokens":32716,"completionTokens":433,"cacheHitTokens":32000,"cacheMissTokens":716,"costUsd":0.00080417}"#,
        ];
        std::fs::write(&usage, format!("{}\n", lines.join("\n"))).unwrap();

        let read = read_new_reasonix_events(&usage, &sessions, 0).unwrap();
        assert_eq!(read.events.len(), 2);
        let e0 = &read.events[0];
        assert_eq!(e0.source, crate::model::Source::DeepSeek);
        assert_eq!(e0.model, "deepseek-v4-flash");
        assert_eq!(e0.session_id, "code-Projects");
        assert_eq!(e0.project, "Projects");
        assert_eq!(e0.usage.input, 23_015);
        assert_eq!(e0.usage.cache_read, 0);
        let e1 = &read.events[1];
        assert_eq!(e1.model, "deepseek-v4-pro");
        assert_eq!(e1.usage.input, 716);
        assert_eq!(e1.usage.cache_read, 32_000);
        assert_eq!(e1.usage.output, 433);
        // Per-line offsets differ -> distinct dedup keys.
        assert_ne!(e0.request_id, e1.request_id);
        assert!(e0.request_id.starts_with("reasonix:code-Projects:"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Project falls back to the session name when no meta workspace exists.
    #[test]
    fn reasonix_project_falls_back_to_session_name() {
        let dir = reasonix_tmp();
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let usage = dir.join("usage.jsonl");
        std::fs::write(
            &usage,
            "{\"ts\":1,\"session\":\"solo-Chat\",\"model\":\"deepseek-v4-pro\",\"completionTokens\":5,\"cacheHitTokens\":0,\"cacheMissTokens\":10}\n",
        )
        .unwrap();
        let read = read_new_reasonix_events(&usage, &sessions, 0).unwrap();
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.events[0].project, "solo-Chat");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An incremental tail read only emits the newly-appended turn, and a
    /// half-written trailing line is NOT consumed (re-read once completed).
    #[test]
    fn reasonix_incremental_tail_and_half_line() {
        let dir = reasonix_tmp();
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let usage = dir.join("usage.jsonl");
        let l1 = r#"{"ts":1,"session":"s","model":"deepseek-v4-pro","completionTokens":1,"cacheHitTokens":0,"cacheMissTokens":10}"#;
        std::fs::write(&usage, format!("{l1}\n")).unwrap();
        let first = read_new_reasonix_events(&usage, &sessions, 0).unwrap();
        assert_eq!(first.events.len(), 1);

        // Append a complete turn + a half-written (no newline) trailing line.
        let l2 = r#"{"ts":2,"session":"s","model":"deepseek-v4-pro","completionTokens":2,"cacheHitTokens":0,"cacheMissTokens":20}"#;
        let half = r#"{"ts":3,"session":"s","model":"deepseek-v4-pro"#; // truncated
        let mut f = std::fs::OpenOptions::new().append(true).open(&usage).unwrap();
        use std::io::Write;
        write!(f, "{l2}\n{half}").unwrap();

        let tail = read_new_reasonix_events(&usage, &sessions, first.new_offset).unwrap();
        assert_eq!(tail.events.len(), 1, "only the one COMPLETE appended turn");
        assert_eq!(tail.events[0].ts, 2);
        // The offset must stop before the half-written line (re-read on completion).
        assert_eq!(tail.new_offset, first.new_offset + (l2.len() as u64 + 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backfill is idempotent: a second pass inserts no new rows.
    #[test]
    fn reasonix_backfill_idempotent() {
        let dir = reasonix_tmp();
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let usage = dir.join("usage.jsonl");
        std::fs::write(
            &usage,
            "{\"ts\":1,\"session\":\"s\",\"model\":\"deepseek-v4-pro\",\"completionTokens\":5,\"cacheHitTokens\":40,\"cacheMissTokens\":10}\n",
        )
        .unwrap();

        let store = Store::open_in_memory().unwrap();
        backfill_reasonix(&usage, &sessions, &store, |_, _| {}).unwrap();
        let first = store.event_count().unwrap();
        backfill_reasonix(&usage, &sessions, &store, |_, _| {}).unwrap();
        assert_eq!(first, 1);
        assert_eq!(first, store.event_count().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backfill against the real `~/.reasonix/usage.jsonl` if present; skip
    /// otherwise. Must not panic and must be idempotent on the real corpus.
    #[test]
    fn reasonix_backfill_real_file_if_present() {
        let usage = match crate::paths::reasonix_usage_path() {
            Ok(p) if p.is_file() => p,
            _ => return, // no Reasonix data on this machine: skip
        };
        let sessions = crate::paths::reasonix_sessions_dir().unwrap();
        let store = Store::open_in_memory().unwrap();
        backfill_reasonix(&usage, &sessions, &store, |_, _| {}).unwrap();
        let first = store.event_count().unwrap();
        backfill_reasonix(&usage, &sessions, &store, |_, _| {}).unwrap();
        assert_eq!(first, store.event_count().unwrap());
    }

    /// REAL-DATA validation (read-only): parse the actual
    /// `~/.reasonix/usage.jsonl` and print the DeepSeek events + token totals.
    /// Confirms tokens are non-zero and the model is `deepseek-v4-*`. Ignored by
    /// default; run locally with:
    ///   cargo test -p claude-monitor-core real_reasonix_usage -- --ignored --nocapture
    #[test]
    #[ignore = "reads the real ~/.reasonix corpus; run locally with --ignored --nocapture"]
    fn real_reasonix_usage() {
        let usage = match crate::paths::reasonix_usage_path() {
            Ok(p) if p.is_file() => p,
            _ => {
                println!("\n--- REAL Reasonix: no usage.jsonl on this machine ---\n");
                return;
            }
        };
        let sessions = crate::paths::reasonix_sessions_dir().unwrap();
        let read = read_new_reasonix_events(&usage, &sessions, 0).unwrap();

        let mut tot_input = 0i64;
        let mut tot_cache_read = 0i64;
        let mut tot_output = 0i64;
        println!(
            "\n--- REAL Reasonix usage events ({}) ---",
            read.events.len()
        );
        let prices = crate::pricing::PriceTable::seeded();
        for e in &read.events {
            tot_input += e.usage.input;
            tot_cache_read += e.usage.cache_read;
            tot_output += e.usage.output;
            let cost = prices.cost_usd(&e.usage, &e.model);
            println!(
                "{}  model={}  input={} cache_read={} output={}  total={}  cost={:?}",
                e.request_id,
                e.model,
                e.usage.input,
                e.usage.cache_read,
                e.usage.output,
                e.usage.total(),
                cost
            );
            assert!(!e.model.contains('\u{FFFD}'), "model must be clean UTF-8");
        }
        let grand_total = tot_input + tot_cache_read + tot_output;
        println!(
            "--- totals: input={tot_input} cache_read={tot_cache_read} output={tot_output} grand_total={grand_total} ---\n"
        );
        if !read.events.is_empty() {
            assert!(grand_total > 0, "real DeepSeek token totals must be non-zero");
            assert!(
                read.events.iter().all(|e| e.model.starts_with("deepseek-v4")),
                "every real event must carry a deepseek-v4-* model"
            );
            assert!(
                read.events.iter().all(|e| e.source == crate::model::Source::DeepSeek),
                "every real event must be tagged Source::DeepSeek"
            );
        }
    }
}
