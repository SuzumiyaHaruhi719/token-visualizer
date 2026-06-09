//! Tolerant jsonl line parser: one line -> [`LineKind`].
//!
//! Every error path collapses to [`LineKind::Other`]; this function NEVER
//! panics on invalid, partial, or unexpected input. Half-written JSON (the tail
//! of a file being appended to) parses as `Other`, which the importer/watcher
//! treat as "do not advance the offset".

use chrono::DateTime;
use serde_json::Value;

use crate::model::{AssistantContent, LineKind, ParsedEvent, Usage};
use crate::paths::project_name_from_cwd;
use crate::text::{clean_user_text, is_injected_user_text};

/// Parse a single jsonl line into a [`LineKind`]. Tolerant: any failure yields
/// [`LineKind::Other`].
pub fn parse_line(line: &str) -> LineKind {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return LineKind::Other,
    };
    classify(&v)
}

/// Classify an already-parsed JSON value.
///
/// Real Claude Code attaches `message.usage` to EVERY assistant message — pure
/// reasoning, tool calls, and text alike — so usage alone cannot tell us what the
/// model is doing. We therefore inspect the content blocks FIRST to find the
/// dominant activity (tool_use > thinking > text), then, if the line also carries
/// usage, fold that activity into [`LineKind::Assistant`] (which stays billable).
/// A `tool_result` block (always on a `user` line, never billed) short-circuits.
fn classify(v: &Value) -> LineKind {
    let message = v.get("message");
    let usage_val = message
        .and_then(|m| m.get("usage"))
        .filter(|u| u.is_object());

    let is_user = is_user_line(v, message);

    // A tool_result block lives on a non-billable user line; surface it directly
    // so the state machine can pair it against its tool_use.
    if let Some(content) = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        if usage_val.is_none() {
            if let Some(block) = find_block(content, "tool_result") {
                let tool_use_id = str_field(block, "tool_use_id").unwrap_or_default();
                return LineKind::ToolResult { tool_use_id };
            }
        }

        // A user line whose content is a real text block (a typed prompt, not a
        // tool result handled above) becomes a UserText line.
        if is_user {
            if let Some(text) = user_text_from_array(v, content) {
                return LineKind::UserText(text);
            }
        }

        // Determine the dominant assistant activity from the content blocks.
        if let Some(content_kind) = assistant_content(content) {
            return match usage_val {
                Some(u) => LineKind::Assistant {
                    event: build_event(v, u),
                    content: content_kind,
                },
                None => bare_line_kind(content_kind),
            };
        }
    }

    // A user line whose `message.content` is a plain STRING (the common typed
    // prompt shape) — surfaced as UserText when it is a real prompt.
    if is_user {
        if let Some(s) = message.and_then(|m| m.get("content")).and_then(Value::as_str) {
            if let Some(text) = real_user_text(v, s) {
                return LineKind::UserText(text);
            }
        }
    }

    // Usage with no recognizable content block (rare): still bill it as text.
    if let Some(u) = usage_val {
        return LineKind::Assistant {
            event: build_event(v, u),
            content: AssistantContent::Text,
        };
    }

    // end_turn with no tool_use / content found above.
    if message
        .and_then(|m| m.get("stop_reason"))
        .and_then(Value::as_str)
        == Some("end_turn")
    {
        return LineKind::EndTurn;
    }

    LineKind::Other
}

/// Whether this line represents a USER message. Real Claude jsonl marks it with
/// `message.role == "user"`; we also accept a top-level `type == "user"` as a
/// fallback (some records carry only the top-level marker) — but a `tool_result`
/// content block is filtered out earlier, so this only gates real prompt text.
fn is_user_line(v: &Value, message: Option<&Value>) -> bool {
    let role_is_user = message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("user");
    let type_is_user = v.get("type").and_then(Value::as_str) == Some("user");
    role_is_user || type_is_user
}

/// Extract a real user prompt from an array `content`: the first `text` block
/// whose text is a genuine prompt (cleaned, capped). `None` when there is no
/// text block or it is an injected wrapper / image placeholder.
fn user_text_from_array(v: &Value, content: &[Value]) -> Option<String> {
    let block = find_block(content, "text")?;
    let raw = block.get("text").and_then(Value::as_str)?;
    real_user_text(v, raw)
}

/// Clean + validate a raw user string into a real prompt, or `None` when it is
/// metadata (`isMeta`) or an injected wrapper (`<command-message>`,
/// `<environment_context>`, `[Request interrupted ...]`, skill preamble, ...).
fn real_user_text(v: &Value, raw: &str) -> Option<String> {
    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if is_injected_user_text(raw) {
        return None;
    }
    let cleaned = clean_user_text(raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// The dominant assistant activity in a content array: a `tool_use` call wins
/// (the model is running a tool), then `thinking` (reasoning), then visible
/// `text`. Returns `None` when no recognized assistant block is present.
fn assistant_content(content: &[Value]) -> Option<AssistantContent> {
    if let Some(block) = find_block(content, "tool_use") {
        let id = str_field(block, "id").unwrap_or_default();
        let name = str_field(block, "name").unwrap_or_default();
        return Some(AssistantContent::Tool { id, name });
    }
    if find_block(content, "thinking").is_some() {
        return Some(AssistantContent::Thinking);
    }
    if find_block(content, "text").is_some() {
        return Some(AssistantContent::Text);
    }
    None
}

/// Map an assistant content kind to the bare (no-usage) [`LineKind`] used for
/// streaming lines that have not yet been finalized with a usage block.
fn bare_line_kind(content: AssistantContent) -> LineKind {
    match content {
        AssistantContent::Tool { id, name } => LineKind::ToolUse { id, name },
        AssistantContent::Thinking => LineKind::Thinking,
        AssistantContent::Text => LineKind::EndTurn,
    }
}

/// First content block whose `type` equals `kind`.
fn find_block<'a>(content: &'a [Value], kind: &str) -> Option<&'a Value> {
    content
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some(kind))
}

/// Build a [`ParsedEvent`] from an assistant value and its `usage` object.
fn build_event(v: &Value, usage_val: &Value) -> ParsedEvent {
    let message = v.get("message");

    let request_id = str_field(v, "requestId")
        .or_else(|| str_field(v, "uuid"))
        .unwrap_or_default();

    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso_millis)
        .unwrap_or(0);

    let session_id = str_field(v, "sessionId").unwrap_or_default();

    let project = v
        .get("cwd")
        .and_then(Value::as_str)
        .map(project_name_from_cwd)
        .unwrap_or_else(|| "unknown".to_string());

    let model = message
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    ParsedEvent {
        request_id,
        ts,
        session_id,
        project,
        model,
        usage: parse_usage(usage_val),
        source: crate::model::Source::Claude,
    }
}

/// Extract the token-usage fields, defaulting any missing field to 0.
fn parse_usage(usage: &Value) -> Usage {
    let server = usage.get("server_tool_use");
    Usage {
        input: int_field(usage, "input_tokens"),
        output: int_field(usage, "output_tokens"),
        cache_create: int_field(usage, "cache_creation_input_tokens"),
        cache_read: int_field(usage, "cache_read_input_tokens"),
        web_search: server
            .map(|s| int_field(s, "web_search_requests"))
            .unwrap_or(0),
        web_fetch: server
            .map(|s| int_field(s, "web_fetch_requests"))
            .unwrap_or(0),
    }
}

/// Parse an ISO-8601 timestamp into epoch milliseconds (UTC).
fn parse_iso_millis(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Read an integer field, tolerating ints, floats, and numeric strings.
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

/// How many bytes the backward scanner reads per chunk. A jsonl line is one
/// record; chunks are reassembled on `\n` boundaries so a line split across two
/// chunks is never parsed truncated.
const BACKWARD_CHUNK_BYTES: u64 = 256 * 1024;

/// Hard safety guard on a single backward cold scan. A cold scan reads back to
/// BOF to find the last real prompt (a 58MB session's prompt can sit megabytes
/// before the tail), but a pathologically huge file is bounded here so the 1s
/// poll can never stall unbounded — and on overflow we return NOTHING rather
/// than leak a stale/injected line. 256MB covers every realistic session jsonl.
pub const MAX_BACKWARD_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// Find the newest real USER prompt in a Claude session jsonl by scanning
/// COMPLETE lines newest-first from EOF down to `floor`.
///
/// * Cold scan: pass `floor = 0` to search the whole file (bounded by
///   [`MAX_BACKWARD_SCAN_BYTES`]); the first [`LineKind::UserText`] found
///   (newest) wins. [`parse_line`] already rejects tool-results and injected
///   wrappers, so any `UserText` here is a genuine, cleaned, capped prompt.
/// * The returned text is always valid UTF-8 (the backward reader decodes per
///   complete line, never mid-codepoint).
///
/// Returns `Ok(None)` when no real prompt exists in `[floor, EOF)` (or the file
/// is larger than the safety guard), `Err` only on an unreadable file.
pub fn last_user_message_backward(
    path: &std::path::Path,
    floor: u64,
) -> std::io::Result<Option<String>> {
    let len = std::fs::metadata(path)?.len();
    // Overflow guard: a file beyond the cap is not cold-scanned in full — return
    // nothing rather than risk an unbounded read or a misleading partial answer.
    let effective_floor = floor.max(len.saturating_sub(MAX_BACKWARD_SCAN_BYTES));
    scan_lines_backward(path, effective_floor, |line| match parse_line(line) {
        LineKind::UserText(text) => Some(text),
        _ => None,
    })
}

/// Scan a file's COMPLETE lines newest-first, invoking `pick` on each fully
/// decoded line until it returns `Some(_)` (early stop) or `[start, len)` is
/// exhausted.
///
/// This is the bounded backward search that finds the last *real* user prompt
/// even when it sits far before the cheap forward tail window (a long
/// assistant/tool turn can push the user's actual prompt megabytes back). It is
/// the read primitive both the Claude poll (`cmserver::state_poll`) and the
/// Codex live reader ([`crate::codex_live`]) use for `last_user_message`.
///
/// Correctness + safety guarantees:
/// * **Whole lines only.** Bytes are read in [`BACKWARD_CHUNK_BYTES`] chunks from
///   the end; a partial line straddling a chunk boundary is carried into the
///   NEXT (earlier) chunk and only emitted once its leading `\n` (or file start)
///   is reached. So `pick` never sees a truncated record.
/// * **Valid UTF-8.** Each emitted line is decoded with `from_utf8_lossy` only
///   AFTER it is delimited on `\n` byte boundaries, so a multibyte codepoint is
///   never split mid-sequence by the chunking (no replacement/surrogate junk
///   from an arbitrary byte cut).
/// * **Bounded.** Never scans before `start`; the caller passes `start = 0` for a
///   cold full scan or `start = cached_len` to scan only the freshly-appended
///   tail. Reads are streamed chunk-by-chunk — never the whole file at once
///   beyond what `pick` needs (it stops at the first match).
///
/// A `start` past the file length (a shrunk/rotated file) is treated as
/// `start = 0` (full scan), never as "skip everything". Returns `Err` only when
/// the file can't be opened/stat'd; a scan that finds no match returns
/// `Ok(None)`.
pub fn scan_lines_backward<T>(
    path: &std::path::Path,
    start: u64,
    mut pick: impl FnMut(&str) -> Option<T>,
) -> std::io::Result<Option<T>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    // A `start` past EOF means the file shrank/rotated since the caller's last
    // observation — fall back to a full scan rather than skipping everything.
    let floor = if start > len { 0 } else { start };
    let mut pos = len;
    // Bytes belonging to a line whose start is earlier than `pos` (i.e. the
    // partial head of the previous chunk), carried down to be prefixed onto the
    // next earlier chunk before splitting.
    let mut carry: Vec<u8> = Vec::new();

    while pos > floor {
        let chunk = BACKWARD_CHUNK_BYTES.min(pos - floor);
        let chunk_start = pos - chunk;
        file.seek(SeekFrom::Start(chunk_start))?;
        let mut buf = vec![0u8; chunk as usize];
        file.read_exact(&mut buf)?;
        pos = chunk_start;

        // Reassemble: this chunk's bytes followed by the carry (which is the
        // start of the line that continued past this chunk's end).
        buf.extend_from_slice(&carry);

        // Split on '\n'. Every segment AFTER the first is a complete line (its
        // leading '\n' is present in this buffer). The first segment may be the
        // tail of a line that starts in an earlier chunk — unless we've reached
        // the floor, in which case it is itself a complete line.
        let mut segments: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
        // The first segment becomes the new carry (it may be incomplete).
        let head = segments.remove(0);
        carry = head.to_vec();

        // Emit complete lines newest-first (segments are file-order, so reverse).
        for seg in segments.iter().rev() {
            if let Some(found) = pick_line(seg, &mut pick) {
                return Ok(Some(found));
            }
        }
    }

    // At the floor: the carry is a complete line (its start is `floor`, a real
    // line boundary when `floor == 0`, or the clean offset boundary the caller
    // guaranteed). Emit it last.
    if !carry.is_empty() {
        if let Some(found) = pick_line(&carry, &mut pick) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Decode one raw line (lossy UTF-8) and offer it to `pick`, skipping blank
/// lines and any trailing `\r` (CRLF-terminated jsonl).
fn pick_line<T>(raw: &[u8], pick: &mut impl FnMut(&str) -> Option<T>) -> Option<T> {
    let text = String::from_utf8_lossy(raw);
    let trimmed = text.trim_end_matches('\r');
    if trimmed.trim().is_empty() {
        return None;
    }
    pick(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file(name: &str, body: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cm-parser-bwd-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body).unwrap();
        path
    }

    #[test]
    fn finds_newest_matching_line_first() {
        // Three "prompt" lines; the scanner must return the LAST (newest) one.
        let body = b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n";
        let path = tmp_file("newest", body);
        let got = scan_lines_backward(&path, 0, |line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("n").and_then(Value::as_i64))
        })
        .unwrap();
        assert_eq!(got, Some(3));
    }

    #[test]
    fn reassembles_lines_across_chunk_boundaries() {
        // A line far longer than a chunk must be reassembled intact, never parsed
        // truncated. Build a >512KB line so it spans multiple 256KB chunks.
        let big = "x".repeat(600 * 1024);
        let body = format!("{{\"tag\":\"first\"}}\n{{\"big\":\"{big}\"}}\n{{\"tag\":\"last\"}}\n");
        let path = tmp_file("bigline", body.as_bytes());

        // Collect every complete line; none should contain a replacement char and
        // the big line must round-trip to valid JSON with the full payload.
        let mut seen: Vec<String> = Vec::new();
        let _: Option<()> = scan_lines_backward(&path, 0, |line| {
            assert!(!line.contains('\u{FFFD}'), "no UTF-8 replacement junk");
            seen.push(line.to_string());
            None
        })
        .unwrap();
        assert_eq!(seen.len(), 3, "exactly three complete lines");
        // Newest-first order.
        assert!(seen[0].contains("\"last\""));
        assert!(seen[2].contains("\"first\""));
        let v: Value = serde_json::from_str(&seen[1]).expect("big line is intact JSON");
        assert_eq!(v.get("big").and_then(Value::as_str).unwrap().len(), 600 * 1024);
    }

    #[test]
    fn reassembles_a_line_spanning_three_chunks() {
        // A ~600KB line sits between two short lines and spans 3 chunks of 256KB.
        // Every line must come back exactly once, in newest-first order, intact.
        let mid = "m".repeat(600 * 1024);
        let body = format!("{{\"a\":1}}\n{{\"mid\":\"{mid}\"}}\n{{\"z\":3}}\n");
        let path = tmp_file("threechunks", body.as_bytes());
        let mut seen = Vec::new();
        let _: Option<()> = scan_lines_backward(&path, 0, |line| {
            seen.push(line.to_string());
            None
        })
        .unwrap();
        assert_eq!(seen.len(), 3, "no dropped or duplicated lines");
        assert!(seen[0].contains("\"z\":3"), "newest first");
        assert!(seen[1].contains("\"mid\""));
        assert!(seen[2].contains("\"a\":1"), "oldest last");
        let v: Value = serde_json::from_str(&seen[1]).unwrap();
        assert_eq!(v.get("mid").and_then(Value::as_str).unwrap().len(), 600 * 1024);
    }

    #[test]
    fn chunk_boundary_exactly_on_newline_does_not_drop_or_dup() {
        // Build a file whose total length is an exact multiple of the chunk size
        // AND whose byte at each chunk boundary is '\n', so a chunk starts cleanly
        // on a line boundary (empty carry). No line may be lost or duplicated.
        let line_len = 64 * 1024; // 64KB payload per line
        let n_lines = (BACKWARD_CHUNK_BYTES as usize / line_len) * 3 + 1;
        let mut body = String::new();
        for i in 0..n_lines {
            body.push_str(&format!(r#"{{"i":{i},"p":"{}"}}"#, "x".repeat(line_len)));
            body.push('\n');
        }
        let path = tmp_file("boundary", body.as_bytes());
        let mut indices = Vec::new();
        let _: Option<()> = scan_lines_backward(&path, 0, |line| {
            let v: Value = serde_json::from_str(line).expect("intact line");
            indices.push(v.get("i").and_then(Value::as_i64).unwrap());
            None
        })
        .unwrap();
        // Exactly every index once, newest-first (descending).
        let mut expected: Vec<i64> = (0..n_lines as i64).rev().collect();
        assert_eq!(indices, expected, "each line exactly once, newest-first");
        expected.sort_unstable();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, expected, "no dup / no drop");
    }

    #[test]
    fn never_splits_a_multibyte_codepoint() {
        // A line of multibyte chars longer than a chunk must decode cleanly when
        // reassembled — the chunk cut lands mid-codepoint but the '\n'-boundary
        // splitting happens on the REASSEMBLED buffer, so no junk leaks.
        let cjk = "中".repeat(300 * 1024); // 3 bytes each => ~900KB, > chunk
        let body = format!("{{\"t\":\"{cjk}\"}}\n");
        let path = tmp_file("cjk", body.as_bytes());
        let got = scan_lines_backward(&path, 0, |line| {
            assert!(!line.contains('\u{FFFD}'), "no replacement char");
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("t").and_then(Value::as_str).map(str::to_string))
        })
        .unwrap();
        assert_eq!(got.unwrap().chars().count(), 300 * 1024);
    }

    #[test]
    fn respects_the_start_floor() {
        // With `start` past the first line, the scan must NOT see it.
        let body = b"{\"n\":1}\n{\"n\":2}\n";
        let path = tmp_file("floor", body);
        let first_line_len = b"{\"n\":1}\n".len() as u64;
        let mut seen = Vec::new();
        let _: Option<()> = scan_lines_backward(&path, first_line_len, |line| {
            seen.push(line.to_string());
            None
        })
        .unwrap();
        assert_eq!(seen, vec!["{\"n\":2}".to_string()]);
    }

    #[test]
    fn missing_file_is_err_not_panic() {
        let path = std::env::temp_dir().join("cm-parser-bwd-does-not-exist.jsonl");
        let r: std::io::Result<Option<()>> = scan_lines_backward(&path, 0, |_| Some(()));
        assert!(r.is_err());
    }

    #[test]
    fn start_past_eof_is_treated_as_full_scan() {
        // A shrunk/rotated file (start > len) must clamp to a full scan, not skip.
        let body = b"{\"n\":7}\n";
        let path = tmp_file("rotated", body);
        let got = scan_lines_backward(&path, 1_000_000, |line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("n").and_then(Value::as_i64))
        })
        .unwrap();
        assert_eq!(got, Some(7));
    }

    #[test]
    fn file_without_trailing_newline_still_yields_last_line() {
        let body = b"{\"n\":1}\n{\"n\":2}"; // no trailing \n
        let path = tmp_file("notrailing", body);
        let got = scan_lines_backward(&path, 0, |line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get("n").and_then(Value::as_i64))
        })
        .unwrap();
        assert_eq!(got, Some(2), "the un-terminated last line is still emitted");
    }

    #[test]
    fn last_user_message_backward_finds_real_prompt_past_tool_result_tail() {
        // The headline Claude bug shape: the real prompt is FAR back, and the
        // file tail is all tool_results + an injected <task-notification> user
        // line. The backward scan must skip the noise and surface the real one.
        let real = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"fix the i18n gap"},"uuid":"u1","timestamp":"2026-06-09T00:00:00Z"}"#;
        let big_assistant = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}],"usage":{{"input_tokens":1}}}}}}"#,
            "z".repeat(400 * 1024)
        );
        let tool_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        let injected = r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>x</task-id>"}}"#;
        let body = format!("{real}\n{big_assistant}\n{tool_result}\n{injected}\n");
        let path = tmp_file("realprompt", body.as_bytes());

        let got = last_user_message_backward(&path, 0).unwrap();
        assert_eq!(got.as_deref(), Some("fix the i18n gap"));
    }

    #[test]
    fn last_user_message_backward_none_when_only_noise() {
        let tool_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        let injected = r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>x</task-id>"}}"#;
        let body = format!("{tool_result}\n{injected}\n");
        let path = tmp_file("noise", body.as_bytes());
        assert_eq!(last_user_message_backward(&path, 0).unwrap(), None);
    }

    /// REAL-DATA validation (read-only): run the production backward scan over
    /// the actual recently-modified `~/.claude/projects/**/*.jsonl` and assert
    /// every surfaced prompt is a clean, non-injected human prompt. Ignored by
    /// default (machine-specific); run locally with:
    ///   cargo test -p claude-monitor-core real_claude_last_user_message -- --ignored --nocapture
    #[test]
    #[ignore = "reads the real ~/.claude corpus; run locally with --ignored --nocapture"]
    fn real_claude_last_user_message() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let recent_window = 6 * 60 * 60 * 1000; // 6h
        let projects = match crate::paths::projects_dir() {
            Ok(d) if d.is_dir() => d,
            _ => return,
        };

        let mut files: Vec<(std::path::PathBuf, i64)> = walkdir::WalkDir::new(&projects)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .filter_map(|p| {
                let m = std::fs::metadata(&p).ok()?.modified().ok()?;
                Some((p, m.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64))
            })
            .filter(|(_, m)| now.saturating_sub(*m) <= recent_window)
            .collect();
        files.sort_by_key(|(_, m)| std::cmp::Reverse(*m));

        println!("\n--- REAL Claude last_user_message (recent sessions) ---");
        let mut printed = 0;
        for (path, _) in files.iter().take(10) {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            if stem.starts_with("agent-") {
                continue; // sub-agent rollout: filtered in production
            }
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let msg = last_user_message_backward(path, 0).ok().flatten();
            match &msg {
                Some(m) => {
                    println!(
                        "[{}…] {:.1} MB  lastUserMessage = {:?}",
                        &stem[..8],
                        len as f64 / 1_048_576.0,
                        m
                    );
                    // Validate: clean UTF-8 and NOT an injected wrapper.
                    assert!(!m.contains('\u{FFFD}'), "prompt must be clean UTF-8");
                    assert!(
                        !crate::text::is_injected_user_text(m),
                        "surfaced prompt must not be an injected wrapper: {m:?}"
                    );
                    assert!(!m.is_empty());
                    printed += 1;
                }
                None => println!("[{}…] {:.1} MB  <none>", &stem[..8], len as f64 / 1_048_576.0),
            }
        }
        println!("--- validated {printed} real Claude prompt(s) ---\n");
    }
}
