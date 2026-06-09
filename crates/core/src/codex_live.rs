//! Live Codex CLI session detection from `~/.codex/sessions` (read-only).
//!
//! The companion of the Claude live-session detection in `cmserver::state_poll`:
//! it enumerates the active Codex rollout files, reads each tail, and derives a
//! [`SessionState`] (with [`Source::Codex`]) per live session. Strictly read-only.
//!
//! ## Why a stateful tracker (not a plain re-walk)
//!
//! Codex writes one rollout file per session, nested `<YYYY>/<MM>/<DD>/`, and a
//! long-lived "resume"d session keeps appending to the file in its ORIGINAL day
//! dir — which can be days or weeks old. A naive "scan today's dir" misses those.
//!
//! Crucially, a directory's mtime does NOT change when an existing file inside it
//! is appended, so gating the deep day-dir walk on directory mtime would miss
//! exactly the long-lived-session case. Instead [`CodexLiveTracker`] keeps a cache
//! of discovered rollout PATHS and re-`stat`s those cheaply every tick (catching
//! appends to old files), while the broader day-dir DISCOVERY walk that finds
//! newly-created rollouts runs only every [`DISCOVERY_INTERVAL_MS`]. A file is
//! "live" purely by its mtime being within [`ACTIVE_WINDOW_MS`] — robust to the
//! 15-60s no-write reasoning gaps that make a per-line heartbeat look stale.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use walkdir::WalkDir;

use crate::codex::{self, CodexActivity, CodexLine};
use crate::model::{SessionState, Source};
use crate::paths::project_name_from_cwd;

/// A rollout whose file mtime is within this window is treated as a live session.
/// Matches the Claude side's `ACTIVE_WINDOW_MS` and clawd's 5-min window, so a
/// long model reasoning gap (no writes for 15-60s) never drops the session.
pub const ACTIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// How many trailing bytes of a rollout to scan for the recent activity window.
const TAIL_BYTES: u64 = 64 * 1024;

/// Byte cap for the one-time head metadata scan. Real rollout head lines are
/// large (a `session_meta` with `base_instructions` is ~22KB; the first
/// `turn_context` carrying the model can begin past byte 48KB), so this is sized
/// to comfortably clear the metadata + first user prompt without reading the
/// whole file. The scan also stops early once model + first prompt are found.
const HEAD_SCAN_BYTES: u64 = 512 * 1024;

/// Line cap for the head scan: a hard stop so a rollout that somehow never
/// declares a model can't make the scan walk the entire file. The model +
/// session_meta + first user prompt are always within the first handful of lines.
const HEAD_SCAN_LINES: usize = 64;

/// How often the deep day-dir DISCOVERY walk runs (to find newly-created rollout
/// files). Between discoveries, cached paths are re-stat'd every tick so appends
/// to already-known files are still seen at the full poll cadence.
const DISCOVERY_INTERVAL_MS: i64 = 30_000;

/// How long a discovered rollout path is kept in the re-stat cache after its
/// last write before being evicted. Generously longer than [`ACTIVE_WINDOW_MS`]
/// so a session that pauses just past the live window is still re-stat'd (and
/// re-activates instantly if it writes again) without a fresh deep walk, but
/// short enough that `known` never accumulates stale historical rollouts.
const CACHE_TTL_MS: i64 = 30 * 60 * 1000;

/// Stateful live-Codex detector. One instance lives in the poll loop; call
/// [`CodexLiveTracker::poll`] each tick to get the current live Codex sessions.
#[derive(Debug, Default)]
pub struct CodexLiveTracker {
    /// Rollout paths discovered so far (re-stat'd cheaply every tick).
    known: HashSet<PathBuf>,
    /// Epoch-ms of the last deep day-dir discovery walk (0 = never).
    last_discovery_ms: i64,
}

impl CodexLiveTracker {
    /// Create an empty tracker (no rollouts discovered yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// One polling pass: return the live Codex sessions (those whose rollout was
    /// written within [`ACTIVE_WINDOW_MS`]). Read-only; tolerant of a missing dir.
    ///
    /// * `sessions_dir` — `~/.codex/sessions`.
    /// * `now_ms` — current epoch millis.
    /// * `tokens_for` — running token total for a session id (from the store).
    pub fn poll(
        &mut self,
        sessions_dir: &Path,
        now_ms: i64,
        tokens_for: impl Fn(&str) -> i64,
    ) -> Vec<SessionState> {
        if !sessions_dir.is_dir() {
            return Vec::new();
        }

        // Periodically rediscover the recently-written rollout files (catches
        // newly created sessions); between walks we keep re-stat'ing the cached
        // paths so an append to an already-known OLD day dir is still seen.
        if self.last_discovery_ms == 0
            || now_ms.saturating_sub(self.last_discovery_ms) >= DISCOVERY_INTERVAL_MS
        {
            self.discover(sessions_dir, now_ms);
            self.last_discovery_ms = now_ms;
        }

        let mut out = Vec::new();
        let mut drop_paths: Vec<PathBuf> = Vec::new();
        for path in &self.known {
            let mtime = match file_mtime_ms(path) {
                Some(m) => m,
                None => {
                    // File vanished (rotated/deleted) — drop from the cache.
                    drop_paths.push(path.clone());
                    continue;
                }
            };
            let idle_for = now_ms.saturating_sub(mtime);
            if idle_for > ACTIVE_WINDOW_MS {
                // Not live this tick. Keep it cached briefly (cheap re-stat picks
                // up a resumed session that writes again), but evict once it is
                // well past the window so `known` stays bounded to ~recent files
                // even on a machine with hundreds of historical rollouts.
                if idle_for > CACHE_TTL_MS {
                    drop_paths.push(path.clone());
                }
                continue;
            }
            if let Some(session) = read_session(path, now_ms, &tokens_for) {
                out.push(session);
            }
        }
        for p in drop_paths {
            self.known.remove(&p);
        }
        out
    }

    /// Walk `sessions_dir` for `rollout-*.jsonl` files written within
    /// [`ACTIVE_WINDOW_MS`] and merge them into the cache. Only recently-written
    /// files are tracked (an older file can't be live this tick, and would be
    /// re-discovered well within the window if it resumes), so `known` stays
    /// bounded to the handful of recent rollouts rather than every historical
    /// one. Files already in the cache are re-stat'd every tick regardless.
    fn discover(&mut self, sessions_dir: &Path, now_ms: i64) {
        for entry in WalkDir::new(sessions_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if !is_rollout(path) {
                continue;
            }
            let recent = file_mtime_ms(path)
                .map(|m| now_ms.saturating_sub(m) <= ACTIVE_WINDOW_MS)
                .unwrap_or(false);
            if recent {
                self.known.insert(path.to_path_buf());
            }
        }
    }
}

/// Whether a path is a Codex rollout jsonl (`rollout-*.jsonl`).
fn is_rollout(path: &Path) -> bool {
    path.extension().map(|x| x == "jsonl").unwrap_or(false)
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("rollout-"))
            .unwrap_or(false)
}

/// Read one live rollout's tail and build its [`SessionState`]. Returns `None`
/// only when the tail can't be read at all.
fn read_session(
    path: &Path,
    now_ms: i64,
    tokens_for: &impl Fn(&str) -> i64,
) -> Option<SessionState> {
    let mut model = String::new();
    let mut session_id = session_id_from_path(path);
    let mut project = String::new();

    // Metadata (`session_meta` id+cwd, first `turn_context` model) AND the first
    // real user prompt are written at the file HEAD. On a real rollout the head
    // lines are HUGE (a `session_meta` carrying `base_instructions` is ~22KB, and
    // `turn_context` with the model can start past byte 48KB), so a fixed-byte
    // head read misses them — both `model` and the prompt land in the dead zone
    // between a small head window and the tail. Scan COMPLETE head lines instead,
    // bounded by line + byte caps, stopping once model + the first prompt are in
    // hand. (Codex's review: do NOT reuse `latest_snapshot`'s model — it reads
    // the newest rollout GLOBALLY and would mislabel a parallel session.)
    let mut last_user_message = String::new();
    scan_head_meta(
        path,
        &mut model,
        &mut session_id,
        &mut project,
        &mut last_user_message,
    );

    // The tail carries the recent ACTIVITY and the newest line timestamp; a
    // fresher `turn_context` / `session_meta` here overrides the head, and a
    // NEWER user prompt (an interactive session's later turn) overrides the head
    // prompt. The head already supplied the first prompt for one-shot
    // (`codex_exec`) rollouts whose only prompt sits before the tail window.
    let text = read_tail(path)?;
    let mut activities: Vec<CodexActivity> = Vec::new();
    let mut last_ts = 0i64;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        scan_meta(trimmed, &mut model, &mut session_id, &mut project);
        if let Some(ts) = ts_millis(trimmed) {
            last_ts = last_ts.max(ts);
        }
        // A fresher real user prompt in the tail wins over the head one (injected
        // wrappers filtered out).
        if let Some(text) = codex::codex_user_text(trimmed) {
            last_user_message = text;
        }
        // Live activity signal (reasoning / tool call / output / message).
        let activity = codex::codex_activity(trimmed);
        if activity != CodexActivity::Other {
            activities.push(activity);
        }
    }

    // Last activity = newest line ts in the tail, falling back to the file mtime
    // (so a tail with no timestamped lines still has a sane freshness value).
    let last_activity = if last_ts > 0 {
        last_ts
    } else {
        file_mtime_ms(path).unwrap_or(now_ms)
    };

    // State is derived purely from what the recent tail says — NO age-based snap.
    // Liveness (the 5-min active window) is enforced by the tracker's poll loop
    // BEFORE this runs, so an active-but-quiet Codex turn keeps its real
    // Working/Thinking state instead of decaying to Idle. `updated_at` carries
    // the last-activity timestamp so the UI shows "Nm ago" staleness instead.
    let state = codex::derive_codex_state(&activities);
    if project.is_empty() {
        project = "unknown".to_string();
    }
    let tokens = tokens_for(&session_id);

    Some(SessionState {
        session_id,
        project,
        model,
        state,
        tokens,
        updated_at: last_activity,
        last_user_message,
        source: Source::Codex,
    })
}

/// Read at most [`TAIL_BYTES`] from the end of `path` as UTF-8 (lossy). The first
/// (possibly partial) line is fine — it's dropped by the per-line parse. Returns
/// `None` when the file can't be opened.
///
/// Only the tail is parsed here (the recent activity window); the one-time head
/// metadata is read separately by [`scan_head_meta`], so `model` / `project`
/// survive a long rollout. A single tool-output line larger than the window is
/// unparseable, which leaves its (small, in-window) `function_call` unmatched →
/// the session shows Working rather than Thinking — an active state, not a missed one.
fn read_tail(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Scan COMPLETE lines from the START of `path` for the one-time head metadata —
/// `model` (`turn_context`), `session_id` + `project` (`session_meta`), and the
/// FIRST real user prompt — updating each in place. Reads line-by-line (never
/// truncating a record) and stops as soon as both the model and a prompt are
/// captured, or [`HEAD_SCAN_LINES`] / [`HEAD_SCAN_BYTES`] is reached.
///
/// This replaces a fixed-byte head read: real rollout head lines are large
/// enough (22KB+ `session_meta`, `turn_context` starting past 48KB) that a small
/// fixed window misses the model and prompt entirely. Each line is decoded
/// `from_utf8_lossy` AFTER the `\n` split, so a multibyte codepoint is never cut.
/// Tolerant: an unreadable file simply leaves the fields untouched.
fn scan_head_meta(
    path: &Path,
    model: &mut String,
    session_id: &mut String,
    project: &mut String,
    first_user_message: &mut String,
) {
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut scanned = 0u64;
    let mut lines = 0usize;

    while lines < HEAD_SCAN_LINES && scanned < HEAD_SCAN_BYTES {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break, // EOF or read error
            Ok(n) => n,
        };
        scanned += n as u64;
        lines += 1;
        let text = String::from_utf8_lossy(&buf);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        scan_meta(trimmed, model, session_id, project);
        // Capture only the FIRST real prompt from the head; later turns (if any)
        // are picked up by the tail scan, which overrides this.
        if first_user_message.is_empty() {
            if let Some(t) = codex::codex_user_text(trimmed) {
                *first_user_message = t;
            }
        }
        // Early out once we have everything the head is responsible for.
        if !model.is_empty() && !first_user_message.is_empty() {
            break;
        }
    }
}

/// Update `model` / `session_id` / `project` from one rollout line's metadata
/// (`turn_context` model, `session_meta` id, `payload.cwd`). Shared by the head
/// (one-time metadata) and tail (a fresher `turn_context` overrides) scans.
fn scan_meta(line: &str, model: &mut String, session_id: &mut String, project: &mut String) {
    match codex::parse_codex_line(line) {
        CodexLine::Model(m) => *model = m,
        CodexLine::SessionMeta { id } => *session_id = id,
        _ => {}
    }
    if project.is_empty() {
        if let Some(cwd) = cwd_of(line) {
            *project = project_name_from_cwd(&cwd);
        }
    }
}

/// The `payload.cwd` of a rollout line, if present (carried by `session_meta` /
/// `turn_context`).
fn cwd_of(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .as_ref()
        .and_then(|v| v.get("payload"))
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Best-effort epoch-millis from a rollout line's top-level `timestamp`.
fn ts_millis(line: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .as_ref()
        .and_then(|v| v.get("timestamp"))
        .and_then(|t| t.as_str())
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|d| d.timestamp_millis())
        })
}

/// Session uuid from a `rollout-<ISO>-<uuid>.jsonl` path (trailing five groups).
/// Mirrors the importer's `session_id_from_rollout` so live + billing agree.
fn session_id_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() >= 5 {
        parts[parts.len() - 5..].join("-")
    } else {
        stem.to_string()
    }
}

/// File modification time in epoch millis.
fn file_mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let dur = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PetState;
    use std::time::SystemTime;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn tmp() -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("cm-codex-live-{}-{now}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn session_id_is_trailing_uuid() {
        let p = Path::new("rollout-2026-06-09T06-45-24-019ea969-7fb3-7433-b7bf-7613ec95521f.jsonl");
        assert_eq!(session_id_from_path(p), "019ea969-7fb3-7433-b7bf-7613ec95521f");
    }

    #[test]
    fn is_rollout_matches_only_rollout_jsonl() {
        assert!(is_rollout(Path::new("x/rollout-a-b-c-d-e.jsonl")));
        assert!(!is_rollout(Path::new("x/other.jsonl")));
        assert!(!is_rollout(Path::new("x/rollout-a.txt")));
    }

    #[test]
    fn cwd_and_ts_extraction() {
        let line = r#"{"timestamp":"2026-06-09T06:45:24.000Z","type":"session_meta","payload":{"id":"i","cwd":"C:\\Users\\Thomas\\Documents\\New project"}}"#;
        assert_eq!(cwd_of(line).as_deref(), Some("C:\\Users\\Thomas\\Documents\\New project"));
        assert!(ts_millis(line).unwrap() > 0);
    }

    #[test]
    fn live_rollout_is_detected_with_thinking_state() {
        let dir = tmp();
        // A rollout: meta (cwd+id), turn_context (model), a tool call that
        // completed (output) -> the model is now reasoning about it -> Thinking.
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:00.000Z","type":"session_meta","payload":{"id":"sess-live","cwd":"C:\\proj\\Widgets"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"turn_context","payload":{"cwd":"C:\\proj\\Widgets","model":"gpt-5.4-codex"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:02.000Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","call_id":"c1","arguments":"{}"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:03.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-00-aaaa-bbbb-cccc-dddd-eeee.jsonl", body);
        // Make it look freshly written.
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 4242);

        assert_eq!(sessions.len(), 1, "the live rollout must be detected");
        let s = &sessions[0];
        assert_eq!(s.session_id, "sess-live"); // session_meta id wins over filename
        assert_eq!(s.project, "Widgets");
        assert_eq!(s.model, "gpt-5.4-codex");
        assert_eq!(s.state, PetState::Thinking);
        assert_eq!(s.tokens, 4242);
        assert_eq!(s.source, Source::Codex);
    }

    #[test]
    fn stale_rollout_is_not_live() {
        let dir = tmp();
        let body = concat!(
            r#"{"timestamp":"2020-01-01T00:00:00.000Z","type":"session_meta","payload":{"id":"old","cwd":"C:\\x"}}"#, "\n",
        );
        let path = write(&dir, "rollout-2020-01-01T00-00-00-1111-2222-3333-4444-5555.jsonl", body);
        // Poll with a "now" far past the file mtime + the active window.
        let now = file_mtime_ms(&path).unwrap() + ACTIVE_WINDOW_MS + 60_000;

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert!(sessions.is_empty(), "a rollout older than the window is not live");
    }

    #[test]
    fn missing_dir_yields_no_sessions() {
        let mut tracker = CodexLiveTracker::new();
        let nonexistent = std::env::temp_dir().join("cm-codex-live-does-not-exist-xyz");
        let sessions = tracker.poll(&nonexistent, 1_000_000, |_| 0);
        assert!(sessions.is_empty());
    }

    #[test]
    fn old_rollouts_are_not_tracked_and_cache_stays_bounded() {
        let dir = tmp();
        // One genuinely-old rollout (years stale) + nothing else.
        let old = write(
            &dir,
            "rollout-2020-01-01T00-00-00-1111-2222-3333-4444-5555.jsonl",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"cwd\":\"C:\\\\x\"}}\n",
        );
        // "now" is far past the old file's mtime.
        let now = file_mtime_ms(&old).unwrap() + 10 * 365 * 24 * 60 * 60 * 1000;

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert!(sessions.is_empty(), "an old rollout is not live");
        // The deep discovery must NOT have tracked the stale file (bounded cache).
        assert_eq!(tracker.known.len(), 0, "stale rollouts are never cached");
    }

    #[test]
    fn vanished_file_is_evicted_from_cache() {
        let dir = tmp();
        let body = "{\"timestamp\":\"2026-06-09T06:45:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"cwd\":\"C:\\\\p\"}}\n";
        let path = write(&dir, "rollout-2026-06-09T06-45-00-aaaa-bbbb-cccc-dddd-ffff.jsonl", body);
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        assert_eq!(tracker.poll(&dir, now, |_| 0).len(), 1);
        assert_eq!(tracker.known.len(), 1);

        // Delete the file; the next poll must re-stat, find it gone, and evict it.
        std::fs::remove_file(&path).unwrap();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert!(sessions.is_empty());
        assert_eq!(tracker.known.len(), 0, "a vanished file is dropped from the cache");
    }

    #[test]
    fn working_state_for_unmatched_tool_call() {
        let dir = tmp();
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"turn_context","payload":{"cwd":"C:\\a\\Foo","model":"gpt-5.4-codex"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:05.000Z","type":"response_item","payload":{"type":"function_call","name":"apply_patch","call_id":"c9","arguments":"{}"}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-01-9999-8888-7777-6666-5555.jsonl", body);
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, PetState::Working(Some("apply_patch".into())));
        assert_eq!(sessions[0].project, "Foo");
    }

    #[test]
    fn updated_at_is_last_activity_not_poll_time() {
        let dir = tmp();
        // Newest line ts is 06:45:03Z. `updated_at` must equal THAT, not `now`.
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:00.000Z","type":"session_meta","payload":{"id":"s1","cwd":"C:\\p\\Q"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:03.000Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-00-1234-5678-9abc-def0-1111.jsonl", body);
        let mtime = file_mtime_ms(&path).unwrap();
        // Poll well after the file's last line, but still within the active window.
        let now = mtime + 90_000;
        let expected_ts = chrono::DateTime::parse_from_rfc3339("2026-06-09T06:45:03.000Z")
            .unwrap()
            .timestamp_millis();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].updated_at, expected_ts,
            "updated_at must be the newest line ts, not the poll time"
        );
        assert_ne!(sessions[0].updated_at, now);
    }

    #[test]
    fn active_codex_session_keeps_state_after_long_no_write_gap() {
        // The headline bug, Codex side: a reasoning turn that hasn't written for
        // well over a minute (but within the 5-min window) must STILL read
        // Thinking — never snap to Idle.
        let dir = tmp();
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"turn_context","payload":{"cwd":"C:\\a\\Bar","model":"gpt-5.4-codex"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:02.000Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-01-2222-3333-4444-5555-6666.jsonl", body);
        let mtime = file_mtime_ms(&path).unwrap();
        // 2 minutes since the last write: old `IDLE_MS=60s` would have snapped
        // this to Idle. It must not.
        let now = mtime + 120_000;

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1, "still within the 5-min active window");
        assert_eq!(sessions[0].state, PetState::Thinking);
        assert_ne!(sessions[0].state, PetState::Idle);
    }

    #[test]
    fn extracts_last_user_message_skipping_injected_wrapper() {
        let dir = tmp();
        // First user message is the auto-injected <environment_context> wrapper;
        // the real prompt is the SECOND user message — that is what must surface.
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:00.000Z","type":"session_meta","payload":{"id":"su","cwd":"C:\\p\\R"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>C:\\p\\R</cwd>\n</environment_context>"}]}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Audit the uncommitted change in this repo"}]}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:03.000Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-00-7777-8888-9999-aaaa-bbbb.jsonl", body);
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].last_user_message,
            "Audit the uncommitted change in this repo"
        );
    }

    #[test]
    fn model_and_prompt_survive_huge_head_lines() {
        // The real Codex bug (model="" + last_user_message="" live): a real
        // rollout's `session_meta` line is ~22KB and `turn_context` (model) can
        // start past byte 48KB — both far beyond any small fixed head window, and
        // the one-shot prompt sits right after, BEFORE the 64KB tail. The
        // line-bounded head scan must still capture all of them.
        let dir = tmp();
        let big_instructions = "i".repeat(22 * 1024); // ~22KB session_meta payload
        let big_meta = format!(
            r#"{{"timestamp":"2026-06-09T00:00:00.000Z","type":"session_meta","payload":{{"id":"sess-huge","cwd":"C:\\proj\\Huge","base_instructions":"{big_instructions}"}}}}"#
        );
        // A second large head line to push turn_context past 32KB, mirroring the
        // real rollout layout (user_instructions / environment_context preamble).
        let filler = "f".repeat(25 * 1024);
        let big_filler = format!(
            r#"{{"timestamp":"2026-06-09T00:00:00.100Z","type":"response_item","payload":{{"type":"message","role":"developer","content":[{{"type":"input_text","text":"{filler}"}}]}}}}"#
        );
        let turn_context = r#"{"timestamp":"2026-06-09T00:00:00.200Z","type":"turn_context","payload":{"cwd":"C:\\proj\\Huge","model":"gpt-5.5"}}"#;
        let env_ctx = r#"{"timestamp":"2026-06-09T00:00:00.300Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>C:\\proj\\Huge</cwd>\n</environment_context>"}]}}"#;
        let real_prompt = r#"{"timestamp":"2026-06-09T00:00:00.400Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"CorePilot i18n gap audit"}]}}"#;
        // Then a long tool turn so the prompt is far before the tail window.
        let big_reasoning = format!(
            r#"{{"timestamp":"2026-06-09T00:01:00.000Z","type":"response_item","payload":{{"type":"reasoning","summary":[],"encrypted_content":"{}"}}}}"#,
            "z".repeat(80 * 1024)
        );
        let body = format!(
            "{big_meta}\n{big_filler}\n{turn_context}\n{env_ctx}\n{real_prompt}\n{big_reasoning}\n"
        );
        let path = write(
            &dir,
            "rollout-2026-06-09T00-00-00-1234-5678-9abc-def0-2222.jsonl",
            &body,
        );
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.model, "gpt-5.5", "model must survive the huge head lines");
        assert_eq!(s.session_id, "sess-huge");
        assert_eq!(s.project, "Huge");
        assert_eq!(
            s.last_user_message, "CorePilot i18n gap audit",
            "the real head prompt must surface (injected env_context skipped)"
        );
    }

    /// REAL-DATA validation (read-only): run the production [`read_session`] over
    /// the actual recently-modified `~/.codex/sessions/**/rollout-*.jsonl` and
    /// assert model + last_user_message are populated and clean. Ignored by
    /// default; run locally with:
    ///   cargo test -p claude-monitor-core real_codex_session -- --ignored --nocapture
    #[test]
    #[ignore = "reads the real ~/.codex corpus; run locally with --ignored --nocapture"]
    fn real_codex_session() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let recent_window = 6 * 60 * 60 * 1000; // 6h
        let dir = match crate::paths::codex_sessions_dir() {
            Ok(d) if d.is_dir() => d,
            _ => return,
        };

        let mut files: Vec<(PathBuf, i64)> = WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| is_rollout(p))
            .filter_map(|p| file_mtime_ms(&p).map(|m| (p, m)))
            .filter(|(_, m)| now.saturating_sub(*m) <= recent_window)
            .collect();
        files.sort_by_key(|(_, m)| std::cmp::Reverse(*m));

        println!("\n--- REAL Codex model + last_user_message (recent rollouts) ---");
        let mut with_model = 0;
        let mut with_prompt = 0;
        for (path, mtime) in files.iter().take(10) {
            // read_session is gated on liveness by the tracker; call it directly
            // here with the file's own mtime as `now` so it is always in-window.
            let Some(s) = read_session(path, *mtime, &|_| 0) else {
                continue;
            };
            println!(
                "{}  model={:?}  prompt={:?}",
                &s.session_id[..s.session_id.len().min(13)],
                s.model,
                s.last_user_message
            );
            assert!(
                !s.model.contains('\u{FFFD}') && !s.last_user_message.contains('\u{FFFD}'),
                "model + prompt must be clean UTF-8"
            );
            if !s.model.is_empty() {
                with_model += 1;
            }
            if !s.last_user_message.is_empty() {
                assert!(
                    !crate::text::is_injected_user_text(&s.last_user_message),
                    "surfaced Codex prompt must not be an injected wrapper"
                );
                with_prompt += 1;
            }
        }
        println!("--- {with_model} rollout(s) with model, {with_prompt} with a real prompt ---\n");
        // On a machine that has used Codex recently, model must populate for at
        // least one rollout (the headline bug was model="" for ALL of them).
        if !files.is_empty() {
            assert!(with_model > 0, "at least one recent rollout must yield a model");
        }
    }

    #[test]
    fn fresh_rollout_ending_on_user_prompt_is_thinking_not_idle() {
        // The must-fix case: a live rollout whose NEWEST line is a real user
        // prompt (no model output yet) must read Thinking, never Idle — otherwise
        // a chat the user just messaged would wrongly show idle.
        let dir = tmp();
        let body = concat!(
            r#"{"timestamp":"2026-06-09T06:45:00.000Z","type":"session_meta","payload":{"id":"sp","cwd":"C:\\p\\S"}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>C:\\p\\S</cwd>\n</environment_context>"}]}}"#, "\n",
            r#"{"timestamp":"2026-06-09T06:45:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"please refactor the parser"}]}}"#, "\n",
        );
        let path = write(&dir, "rollout-2026-06-09T06-45-00-cccc-dddd-eeee-ffff-0000.jsonl", body);
        let now = file_mtime_ms(&path).unwrap();

        let mut tracker = CodexLiveTracker::new();
        let sessions = tracker.poll(&dir, now, |_| 0);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, PetState::Thinking);
        assert_ne!(sessions[0].state, PetState::Idle);
        assert_eq!(sessions[0].last_user_message, "please refactor the parser");
    }
}
