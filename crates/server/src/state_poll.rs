//! Periodic derivation of live session state from `~/.claude` (read-only).
//!
//! Every [`POLL_INTERVAL`] this loop:
//! 1. enumerates `sessions/*.json` for coarse busy/idle status + heartbeats,
//! 2. maps each session to its jsonl (`<session_id>.jsonl` under `projects/**`)
//!    and reads the tail for fine-grained [`cmcore::model::LineKind`] signals,
//! 3. derives a [`SessionState`] per session via [`cmcore::state`],
//! 4. when the set changed, updates [`AppState`] and broadcasts `sessions`/`usage`.
//!
//! When `sessions/` is empty or missing, it falls back to treating
//! recently-modified jsonl files as the active sessions. Strictly read-only.
//!
//! ## Headless vs. desktop
//!
//! The poll + publish path (filesystem → `AppState` + SSE) is GUI-free and lives
//! here so both `cm-serve` (browser mode) and the Tauri app share it verbatim.
//! Desktop side effects — popover show/hide on the monitor toggle, the tray
//! tooltip refresh, and session-end notifications — are abstracted behind
//! [`StatePollHooks`]. The Tauri shell supplies an implementation that captures
//! its `AppHandle`; `cm-serve` passes [`NoopHooks`], so browser mode publishes
//! identical SSE while doing nothing GUI-related.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use cmcore::codex_live::CodexLiveTracker;
use cmcore::importer::read_new_complete_lines;
use cmcore::model::{LineKind, SessionState, Source};
use cmcore::parser::last_user_message_backward;
use cmcore::state::{self, SessionStatus};
use cmcore::store::Store;
use walkdir::WalkDir;

use crate::server::{AppState, SseEvent};
use crate::session_end::{EndedSession, SessionEndTracker};

/// How often the loop recomputes session state.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// How many trailing bytes of a session jsonl to scan for recent line kinds.
const TAIL_BYTES: u64 = 64 * 1024;

/// A jsonl modified within this window (no status file) is treated as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// Per-session cache of the last real user prompt + the byte offset already
/// consumed, so the expensive backward search runs ONCE per session (cold) and
/// every later tick only forward-scans the freshly-appended `[consumed, EOF)`.
///
/// Keyed by `session_id`; `path` is stored so a stale cache for a recycled id is
/// rebuilt if the file path ever changes underneath it.
#[derive(Debug, Clone)]
struct CachedUserMessage {
    /// The jsonl this entry was computed from (cache is reset if the path moves).
    path: PathBuf,
    /// Newest real user prompt seen so far (empty when none found yet).
    message: String,
    /// Byte offset after the last COMPLETE line consumed; the next tick scans
    /// only `[consumed_offset, EOF)`. Per Codex's review this is the
    /// last-newline offset (NOT raw file length) so a half-written trailing line
    /// is re-read once it completes.
    consumed_offset: u64,
}

/// The per-session last-user-message cache (lives for the poll loop's lifetime).
type UserMessageCache = HashMap<String, CachedUserMessage>;

/// A live `session_id` shaped like `agent-...` is a dispatched Task SUB-agent,
/// not a user chat — real Claude sessions are UUIDs. These are filtered from the
/// live list (in both the status-file and recent-jsonl paths).
fn is_subagent_session(session_id: &str) -> bool {
    session_id.starts_with("agent-")
}

/// Resolve the last real user prompt for a session, using + updating the cache.
///
/// * Cold (no cache entry, or the file path changed): backward-scan the whole
///   file (bounded) for the newest real prompt, then remember the offset after
///   the last COMPLETE line as the consumed offset.
/// * Warm: if the file shrank below the cached offset it rotated → cold rescan;
///   otherwise forward-scan only the new `[consumed_offset, EOF)` bytes, adopt a
///   newer prompt if one appeared, and always advance the consumed offset.
///
/// Strictly read-only. On any IO error the previous cached value (or empty) is
/// returned — a transient read failure never blanks a known prompt.
fn last_user_message_cached(path: &Path, cache: &mut UserMessageCache, session_id: &str) -> String {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let needs_cold = match cache.get(session_id) {
        None => true,
        Some(c) => c.path != path || file_len < c.consumed_offset,
    };

    if needs_cold {
        let message = last_user_message_backward(path, 0)
            .ok()
            .flatten()
            .unwrap_or_default();
        cache.insert(
            session_id.to_string(),
            CachedUserMessage {
                path: path.to_path_buf(),
                message: message.clone(),
                // Record the offset AFTER the last COMPLETE line, NOT raw EOF: if
                // the file currently ends on a half-written line, caching EOF
                // would make the next tick's forward scan start past that line and
                // skip it once it completes. (Codex audit nit.)
                consumed_offset: offset_after_last_complete_line(path, file_len),
            },
        );
        return message;
    }

    // Warm path: only the freshly-appended tail can contain a newer prompt.
    let entry = cache.get(session_id).expect("checked present above").clone();
    match read_new_complete_lines(path, entry.consumed_offset) {
        Ok(read) => {
            let newer = read.lines.iter().rev().find_map(|k| match k {
                LineKind::UserText(text) => Some(text.clone()),
                _ => None,
            });
            let message = newer.unwrap_or(entry.message);
            cache.insert(
                session_id.to_string(),
                CachedUserMessage {
                    path: path.to_path_buf(),
                    message: message.clone(),
                    // Always advance even when no prompt appeared, so a quiet tail
                    // is not re-scanned next tick. `new_offset` stops at the last
                    // complete newline, so a half-written line is re-read later.
                    consumed_offset: read.new_offset,
                },
            );
            message
        }
        // Transient read failure: keep the known prompt rather than blank it.
        Err(_) => entry.message,
    }
}

/// Byte offset just after the file's last COMPLETE (newline-terminated) line —
/// always a TRUE line boundary, never mid-record.
///
/// Used to seed the cold-scan `consumed_offset` so a half-written trailing line
/// is re-read once it completes (caching raw EOF would skip it). Reads only the
/// trailing [`TAIL_BYTES`] window to find the last `\n`.
///
/// Critically, if NO `\n` is found in that window it returns `0`, NOT the window
/// start: a window start can fall in the MIDDLE of a record when the trailing
/// unterminated line is itself larger than the window (e.g. a huge pasted
/// prompt). Seeding a mid-record offset would make the next forward scan parse
/// only the suffix as `Other` and advance past it, permanently missing that
/// prompt (Codex audit, REQUEST-CHANGES). Returning `0` makes the next tick do
/// one more (correct) cold backward scan — rare and cheap, never a missed prompt.
/// Falls back to `file_len` only on a read error (no worse than raw EOF).
fn offset_after_last_complete_line(path: &Path, file_len: u64) -> u64 {
    use std::io::{Read, Seek, SeekFrom};
    let window = TAIL_BYTES.min(file_len);
    let start = file_len - window;
    let read = (|| -> std::io::Result<u64> {
        let mut file = std::fs::File::open(path)?;
        if start > 0 {
            file.seek(SeekFrom::Start(start))?;
        }
        let mut buf = vec![0u8; window as usize];
        file.read_exact(&mut buf)?;
        // Last '\n' in the window => everything up to and including it is complete.
        match buf.iter().rposition(|b| *b == b'\n') {
            Some(idx) => Ok(start + idx as u64 + 1),
            // No newline anywhere in the window: the window start may be mid-record
            // (the trailing unterminated line exceeds the window), so the only
            // guaranteed line boundary is BOF. Re-cold-scan next tick.
            None => Ok(0),
        }
    })();
    read.unwrap_or(file_len)
}

/// Live notification preferences passed to [`StatePollHooks::sessions_ended`].
/// Read fresh each tick from the runtime atomics so settings changes apply at
/// once. Volume is a `0.0..=1.0` float.
#[derive(Debug, Clone, Copy)]
pub struct NotificationPrefs {
    pub notifications_enabled: bool,
    pub sound_enabled: bool,
    pub volume: f32,
}

/// Desktop side effects the poll loop fires. All methods have default no-op
/// bodies so a headless caller can implement nothing (or use [`NoopHooks`]).
///
/// Implementations may be called from the poll thread; the Tauri impl marshals
/// to the main thread internally (`AppHandle::run_on_main_thread`) where Windows
/// window ops are safe.
pub trait StatePollHooks: Send + 'static {
    /// The `monitor_enabled` toggle flipped (settings panel). `enabled` is the
    /// new value. Desktop: show/hide the tray popover.
    fn monitor_changed(&self, _enabled: bool) {}

    /// The live-session set changed this tick. `sessions` is most-recently-
    /// updated first. Desktop: refresh the tray tooltip from `sessions.first()`.
    fn sessions_changed(&self, _sessions: &[SessionState]) {}

    /// One or more sessions ended this tick. Desktop: taskbar flash + toast +
    /// chime per ended session, gated by `prefs`.
    fn sessions_ended(&self, _ended: &[EndedSession], _prefs: NotificationPrefs) {}
}

/// A hooks implementation that does nothing — used by `cm-serve` (browser mode),
/// which publishes SSE but has no tray/popover/notifications.
pub struct NoopHooks;
impl StatePollHooks for NoopHooks {}

/// Run the poll loop forever. Spawn this on a dedicated OS thread; it opens its
/// own read `Store` per pass (never shares one across threads).
///
/// The broadcast + shared state are updated every changed tick regardless of
/// `hooks`; `hooks` carries the (optional) desktop side effects.
pub fn run<H: StatePollHooks>(state: AppState, hooks: H) {
    // Cache of session_id -> jsonl path so we don't re-walk the whole tree
    // every second. Paths are stable once discovered.
    let mut path_cache: HashMap<String, PathBuf> = HashMap::new();
    // Cache of session_id -> last real user prompt + consumed offset, so the
    // backward prompt search runs once per session (then only tail deltas).
    let mut user_msg_cache: UserMessageCache = HashMap::new();
    let mut last_emitted: Vec<SessionState> = Vec::new();
    let mut last_monitor = state.runtime.monitor_enabled.load(Ordering::Relaxed);
    // Tracks the live-session set across ticks to fire session-end notifications.
    let mut end_tracker = SessionEndTracker::new();
    // Live Codex detector: stateful across ticks (caches discovered rollout
    // paths + throttles the deep day-dir discovery walk).
    let mut codex_tracker = CodexLiveTracker::new();

    loop {
        // When the monitor toggle flips (settings panel flips the atomic), let
        // the desktop layer react (show/hide the popover). Headless no-ops.
        let monitor = state.runtime.monitor_enabled.load(Ordering::Relaxed);
        if monitor != last_monitor {
            hooks.monitor_changed(monitor);
        }
        last_monitor = monitor;

        let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match poll_once(
                &state,
                &mut path_cache,
                &mut user_msg_cache,
                &mut codex_tracker,
            ) {
                Ok(sessions) => {
                    // Session-end detection runs EVERY tick (even when the
                    // emitted set is unchanged) so a disappearing session is
                    // never missed by the change short-circuit below.
                    let ended = end_tracker.observe(&sessions);
                    if !ended.is_empty() {
                        // Read the live notification + sound toggles + volume each
                        // time so settings changes take effect immediately.
                        let prefs = NotificationPrefs {
                            notifications_enabled: state
                                .runtime
                                .notifications_enabled
                                .load(Ordering::Relaxed),
                            sound_enabled: state.runtime.sound_enabled.load(Ordering::Relaxed),
                            volume: state.runtime.sound_volume.load(Ordering::Relaxed) as f32
                                / 100.0,
                        };
                        hooks.sessions_ended(&ended, prefs);
                    }

                    if sessions != last_emitted {
                        last_emitted = sessions.clone();
                        publish(&state, sessions.clone());
                        hooks.sessions_changed(&sessions);
                    }
                }
                Err(e) => {
                    // Non-fatal: log and keep polling. A transient FS error must
                    // not kill live updates.
                    eprintln!("[state-poll] {e:#}");
                }
            }
        }));
        if tick.is_err() {
            eprintln!("[state-poll] tick panicked");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One polling pass. Pure-ish: reads the filesystem + store, returns the derived
/// session list (most-recently-updated first).
fn poll_once(
    state: &AppState,
    path_cache: &mut HashMap<String, PathBuf>,
    user_msg_cache: &mut UserMessageCache,
    codex_tracker: &mut CodexLiveTracker,
) -> anyhow::Result<Vec<SessionState>> {
    let now_ms = now_ms();
    let store = Store::open(&state.db_path)?;

    // Status files carry the richest signal, but Claude Code only writes one for
    // SOME live sessions — so deriving from status files alone hides parallel
    // sessions. Start from the status-file sessions, then UNION in any
    // recently-active jsonl whose session id isn't already covered. (With no
    // status files at all this degrades to the pure recent-jsonl heuristic.)
    let statuses = read_session_statuses();
    let mut sessions =
        derive_from_status_files(&store, &statuses, path_cache, user_msg_cache, now_ms)?;
    let mut seen: std::collections::HashSet<String> =
        sessions.iter().map(|s| s.session_id.clone()).collect();
    for s in derive_from_recent_jsonl(&store, user_msg_cache, now_ms)? {
        if seen.insert(s.session_id.clone()) {
            sessions.push(s);
        }
    }

    // UNION in live Codex sessions (read from ~/.codex/sessions, read-only). This
    // is the ONLY path that surfaces Codex as a live session with a live state;
    // it is independent of the Claude status-file / jsonl heuristic above. Codex
    // ids are namespaced uuids, but we still dedup by id for safety.
    if let Ok(codex_dir) = cmcore::paths::codex_sessions_dir() {
        let codex_sessions = codex_tracker.poll(&codex_dir, now_ms, |id| {
            cmcore::query::session_tokens(&store, id).unwrap_or(0)
        });
        for s in codex_sessions {
            if seen.insert(s.session_id.clone()) {
                sessions.push(s);
            }
        }
    }

    // UNION in live Reasonix (DeepSeek) sessions (read from ~/.reasonix/sessions,
    // read-only). Reasonix session names (e.g. `code-Projects`) are short and
    // collision-prone vs Claude/Codex UUIDs, so token lookup is SOURCE-SCOPED to
    // DeepSeek to avoid summing a same-named session of another agent.
    if let Ok(reasonix_dir) = cmcore::paths::reasonix_sessions_dir() {
        let reasonix_sessions = cmcore::reasonix_live::poll(&reasonix_dir, now_ms, |id| {
            cmcore::query::session_tokens_for_source(&store, id, Source::DeepSeek).unwrap_or(0)
        });
        for s in reasonix_sessions {
            if seen.insert(s.session_id.clone()) {
                sessions.push(s);
            }
        }
    }

    // Most-recently-updated first (drives `current` + pet cascade order).
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
    Ok(sessions)
}

/// Update shared state and broadcast `sessions` + `usage`.
fn publish(state: &AppState, sessions: Vec<SessionState>) {
    let current = sessions.first().cloned();

    // Block on the async RwLocks from this sync thread via a tiny runtime-free
    // path: `blocking_write` is provided by tokio's RwLock.
    {
        let mut s = state.sessions.blocking_write();
        *s = sessions.clone();
    }
    {
        let mut c = state.current.blocking_write();
        *c = current.clone();
    }

    let _ = state.tx.send(SseEvent::Sessions(sessions));
    let _ = state.tx.send(SseEvent::Usage(current));
}

/// Read every `sessions/*.json`, returning parsed statuses keyed by file stem
/// (the pid). Missing dir -> empty map (handled by the caller's fallback).
fn read_session_statuses() -> Vec<SessionStatus> {
    let dir = match cmcore::paths::sessions_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(), // dir absent: fall back to jsonl heuristic
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|x| x == "json").unwrap_or(false) {
            let status = state::read_session_status(&path);
            // Skip entries that carry no usable session id.
            if status.session_id.is_some() {
                out.push(status);
            }
        }
    }
    out
}

/// Derive sessions from `sessions/*.json` statuses + jsonl tails.
fn derive_from_status_files(
    store: &Store,
    statuses: &[SessionStatus],
    path_cache: &mut HashMap<String, PathBuf>,
    user_msg_cache: &mut UserMessageCache,
    now_ms: i64,
) -> anyhow::Result<Vec<SessionState>> {
    let mut out = Vec::new();
    for st in statuses {
        let Some(session_id) = st.session_id.clone() else {
            continue;
        };
        // Dispatched Task sub-agents (`agent-...`) are not user chats — skip them.
        if is_subagent_session(&session_id) {
            continue;
        }
        let jsonl = find_session_jsonl(&session_id, path_cache);
        let (project, model, last_activity_ms, recent_lines) = match &jsonl {
            Some(p) => read_tail_signals(p, now_ms),
            None => (
                project_fallback(&session_id),
                String::new(),
                st.updated_at,
                Vec::new(),
            ),
        };

        // Prefer the heartbeat for last activity if it is fresher than the file.
        let last_activity = last_activity_ms.max(st.updated_at);

        // Liveness gate: the core state derivation no longer snaps stale sessions
        // to Idle (that responsibility moved here), so a status file lingering on
        // disk for a long-finished session must be filtered out by its activity
        // age — otherwise it would keep showing its last live state forever.
        if now_ms.saturating_sub(last_activity) > ACTIVE_WINDOW_MS {
            continue;
        }

        let running_tokens = cmcore::query::session_tokens(store, &session_id).unwrap_or(0);

        let mut session = state::session_state_from(
            &session_id,
            &project,
            &model,
            &st.status,
            &recent_lines,
            running_tokens,
            last_activity,
            Source::Claude,
        );
        // The 64KB tail feeds pet-state; the real last prompt may sit far before
        // it, so resolve it via the dedicated cached backward scan (kept separate
        // from `recent_lines` per Codex's review — historical metadata, not state).
        if let Some(p) = &jsonl {
            session.last_user_message = last_user_message_cached(p, user_msg_cache, &session_id);
        }
        out.push(session);
    }
    Ok(out)
}

/// Fallback path: when no status files exist, treat any jsonl modified within
/// [`ACTIVE_WINDOW_MS`] as an active session and derive purely from its tail.
fn derive_from_recent_jsonl(
    store: &Store,
    user_msg_cache: &mut UserMessageCache,
    now_ms: i64,
) -> anyhow::Result<Vec<SessionState>> {
    let projects = match cmcore::paths::projects_dir() {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for path in collect_recent_jsonl(&projects, now_ms) {
        let session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if session_id.is_empty() {
            continue;
        }
        // Dispatched Task sub-agents (`agent-...`) are not user chats — skip them.
        // (Sub-agent rollouts live under a `.../<uuid>/subagents/agent-*.jsonl`
        // path, so their file stem carries the `agent-` prefix.)
        if is_subagent_session(&session_id) {
            continue;
        }
        let (project, model, last_activity_ms, recent_lines) = read_tail_signals(&path, now_ms);
        let running_tokens = cmcore::query::session_tokens(store, &session_id).unwrap_or(0);

        // These files are already filtered to the active window by
        // `collect_recent_jsonl`. No status file -> empty status string -> core
        // derives purely from the tail lines (staleness is the window gate, not a
        // snap), and `updated_at` carries the last-activity ts for "Nm ago".
        let mut session = state::session_state_from(
            &session_id,
            &project,
            &model,
            "",
            &recent_lines,
            running_tokens,
            last_activity_ms,
            Source::Claude,
        );
        // Real last prompt via the cached backward scan (see status-file path).
        session.last_user_message = last_user_message_cached(&path, user_msg_cache, &session_id);
        out.push(session);
    }
    Ok(out)
}

/// Read the tail of a session jsonl: returns `(project, model, last_activity_ms,
/// recent_lines)`. Reads at most [`TAIL_BYTES`] from the end; tolerant of a
/// partial first line. Read-only.
fn read_tail_signals(path: &Path, fallback_ms: i64) -> (String, String, i64, Vec<LineKind>) {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);

    // Reuse the importer's complete-line reader from `start`; the first line may
    // be partial (start is mid-line) — that's fine, it's dropped, not consumed.
    let read = match read_new_complete_lines(path, start) {
        Ok(r) => r,
        Err(_) => {
            return (
                project_fallback(
                    &path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                ),
                String::new(),
                fallback_ms,
                Vec::new(),
            )
        }
    };

    // Project + model from the most recent assistant event; last activity from
    // the NEWEST event timestamp in the tail (seeded at 0, NOT `fallback_ms`, so
    // a real — necessarily past — line timestamp actually wins and the frontend
    // can render "Nm ago"). Events are oldest-first, so the last one is newest.
    let mut project = String::new();
    let mut model = String::new();
    let mut last_activity = 0i64;
    if let Some(newest) = read.events.last() {
        last_activity = newest.ts;
    }
    for ev in read.events.iter().rev() {
        if project.is_empty() {
            project = ev.project.clone();
        }
        if model.is_empty() {
            model = ev.model.clone();
        }
        if !project.is_empty() && !model.is_empty() {
            break;
        }
    }
    if project.is_empty() {
        // Decode the jsonl directory name (e.g. C--Users-...-CorePilot).
        project = project_from_dir(path);
    }
    if last_activity == 0 {
        // No timestamped assistant events in the tail: use the file mtime (then
        // the caller's fallback) as the activity time.
        last_activity = file_mtime_ms(path).unwrap_or(fallback_ms);
    }

    (project, model, last_activity, read.lines)
}

/// Locate `<session_id>.jsonl` under `projects/**`, caching the result.
fn find_session_jsonl(session_id: &str, cache: &mut HashMap<String, PathBuf>) -> Option<PathBuf> {
    if let Some(p) = cache.get(session_id) {
        if p.is_file() {
            return Some(p.clone());
        }
    }
    let projects = cmcore::paths::projects_dir().ok()?;
    let target = format!("{session_id}.jsonl");
    for entry in WalkDir::new(&projects)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if entry.file_name().to_string_lossy() == target {
            let path = entry.into_path();
            cache.insert(session_id.to_string(), path.clone());
            return Some(path);
        }
    }
    None
}

/// Collect jsonl files modified within [`ACTIVE_WINDOW_MS`] of `now_ms`.
fn collect_recent_jsonl(projects: &Path, now_ms: i64) -> Vec<PathBuf> {
    WalkDir::new(projects)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .filter(|p| {
            file_mtime_ms(p)
                .map(|m| now_ms.saturating_sub(m) <= ACTIVE_WINDOW_MS)
                .unwrap_or(false)
        })
        .collect()
}

/// File modification time in epoch millis.
fn file_mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

/// Decode the project name from the jsonl's parent directory name. Claude Code
/// encodes the cwd as `C--Users-Thomas-Documents-CorePilot`; the friendly name
/// is the last hyphen segment.
fn project_from_dir(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| {
            let raw = n.to_string_lossy();
            raw.rsplit('-')
                .find(|s| !s.is_empty())
                .unwrap_or("unknown")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Last-ditch project label when nothing else is available.
fn project_fallback(session_id: &str) -> String {
    if session_id.is_empty() {
        "unknown".to_string()
    } else {
        session_id.chars().take(8).collect()
    }
}

/// Current epoch millis.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_from_encoded_dir() {
        let p = Path::new(r"C:\x\C--Users-Thomas-Documents-CorePilot\abc.jsonl");
        assert_eq!(project_from_dir(p), "CorePilot");
    }

    #[test]
    fn project_fallback_uses_short_id() {
        assert_eq!(project_fallback("0123456789abcdef"), "01234567");
        assert_eq!(project_fallback(""), "unknown");
    }

    #[test]
    fn recent_jsonl_threshold_is_inclusive() {
        let dir = tempfile_dir();
        let file = dir.join("live.jsonl");
        std::fs::write(&file, "").unwrap();
        let mtime = file_mtime_ms(&file).unwrap();

        let at_threshold = collect_recent_jsonl(&dir, mtime + ACTIVE_WINDOW_MS);
        assert_eq!(at_threshold, vec![file.clone()]);

        let past_threshold = collect_recent_jsonl(&dir, mtime + ACTIVE_WINDOW_MS + 1);
        assert!(past_threshold.is_empty());
    }

    fn tempfile_dir() -> PathBuf {
        // A per-call atomic counter guarantees a unique dir even when several
        // tests construct one within the same millisecond on parallel threads
        // (without it, `process::id()` + `now_ms()` collide and tests stomp on
        // each other's files).
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cm-state-poll-test-{}-{}-{}",
            std::process::id(),
            now_ms(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn subagent_sessions_are_filtered() {
        // Dispatched Task sub-agents use `agent-<hex>` ids; real user sessions are
        // UUIDs. Only the `agent-` ones must be filtered out.
        assert!(is_subagent_session("agent-ab35872233409e1de"));
        assert!(!is_subagent_session(
            "c65617ae-56e9-4ce1-8a48-cc4ec5707519"
        ));
        assert!(!is_subagent_session("")); // empty id is not an agent
    }

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{text}"}},"uuid":"u","timestamp":"2026-06-09T00:00:00Z"}}"#
        )
    }

    #[test]
    fn cached_message_cold_then_warm_picks_up_newer_prompt() {
        let dir = tempfile_dir();
        let path = dir.join("sess.jsonl");
        // Cold: one real prompt far behind a big assistant turn.
        let big = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}],"usage":{{"input_tokens":1}}}}}}"#,
            "z".repeat(300 * 1024)
        );
        std::fs::write(&path, format!("{}\n{big}\n", user_line("first prompt"))).unwrap();

        let mut cache: UserMessageCache = HashMap::new();
        let cold = last_user_message_cached(&path, &mut cache, "s");
        assert_eq!(cold, "first prompt", "cold backward scan finds the real prompt");
        let off1 = cache.get("s").unwrap().consumed_offset;

        // Warm: append a NEWER prompt; the next call must adopt it via the delta.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        writeln!(f, "{}", user_line("second prompt")).unwrap();
        let warm = last_user_message_cached(&path, &mut cache, "s");
        assert_eq!(warm, "second prompt", "warm delta scan adopts the newer prompt");
        assert!(cache.get("s").unwrap().consumed_offset > off1, "offset advanced");
    }

    #[test]
    fn cached_message_warm_keeps_prompt_when_only_tool_results_appended() {
        let dir = tempfile_dir();
        let path = dir.join("sess.jsonl");
        std::fs::write(&path, format!("{}\n", user_line("the only prompt"))).unwrap();
        let mut cache: UserMessageCache = HashMap::new();
        assert_eq!(
            last_user_message_cached(&path, &mut cache, "s"),
            "the only prompt"
        );

        // Append only tool-results + an injected wrapper — no new real prompt.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        let tool = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#;
        let injected = r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>x</task-id>"}}"#;
        writeln!(f, "{tool}").unwrap();
        writeln!(f, "{injected}").unwrap();
        assert_eq!(
            last_user_message_cached(&path, &mut cache, "s"),
            "the only prompt",
            "no newer real prompt -> cached one is retained, noise never leaks"
        );
    }

    #[test]
    fn cold_offset_does_not_skip_a_half_written_trailing_line() {
        // Cold scan when the file ends on a HALF-WRITTEN line: the cached offset
        // must stop BEFORE it, so once that line completes (turning into a real
        // prompt) the next tick surfaces it instead of skipping past it.
        let dir = tempfile_dir();
        let path = dir.join("sess.jsonl");
        // A complete prompt line, then a half-written (no trailing '\n') line.
        let half = r#"{"type":"user","message":{"role":"user","content":"second pr"#; // truncated
        std::fs::write(&path, format!("{}\n{half}", user_line("first prompt"))).unwrap();

        let mut cache: UserMessageCache = HashMap::new();
        let cold = last_user_message_cached(&path, &mut cache, "s");
        assert_eq!(cold, "first prompt", "the half-written line is not a prompt yet");
        let off = cache.get("s").unwrap().consumed_offset;
        let first_line_len = format!("{}\n", user_line("first prompt")).len() as u64;
        assert_eq!(
            off, first_line_len,
            "consumed offset stops after the last COMPLETE line, not at raw EOF"
        );

        // Now the half-written line completes into a real prompt.
        std::fs::write(
            &path,
            format!("{}\n{}\n", user_line("first prompt"), user_line("second prompt")),
        )
        .unwrap();
        let warm = last_user_message_cached(&path, &mut cache, "s");
        assert_eq!(
            warm, "second prompt",
            "the completed line must be read, not skipped by a too-far offset"
        );
    }

    #[test]
    fn cold_offset_handles_huge_unterminated_trailing_line() {
        // Codex REQUEST-CHANGES regression: a file LARGER than the 64KB tail
        // window whose trailing half-written line is ALSO larger than the window
        // (a huge pasted prompt mid-write). The cold offset must NOT land
        // mid-record — it falls back to BOF — so when that line completes it is
        // surfaced, never skipped.
        let dir = tempfile_dir();
        let path = dir.join("sess.jsonl");
        // A real prompt, then a complete NON-prompt filler (huge assistant line so
        // the file exceeds 64KB), then a >64KB half-written line with no newline.
        let filler = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{}"}}],"usage":{{"input_tokens":1}}}}}}"#,
            "f".repeat(70 * 1024)
        );
        // Half-written huge prompt: >64KB and NO trailing newline.
        let huge_partial = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{}"#,
            "p".repeat(70 * 1024)
        );
        std::fs::write(
            &path,
            format!("{}\n{filler}\n{huge_partial}", user_line("real early prompt")),
        )
        .unwrap();

        let mut cache: UserMessageCache = HashMap::new();
        let cold = last_user_message_cached(&path, &mut cache, "s");
        assert_eq!(cold, "real early prompt", "the partial huge line is not a prompt yet");
        assert_eq!(
            cache.get("s").unwrap().consumed_offset,
            0,
            "no newline in the 64KB window => fall back to BOF, never a mid-record offset"
        );

        // The huge line completes into a real, distinctive prompt.
        let huge_done = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"huge pasted {}"}}}}"#,
            "p".repeat(70 * 1024)
        );
        std::fs::write(
            &path,
            format!("{}\n{filler}\n{huge_done}\n", user_line("real early prompt")),
        )
        .unwrap();
        let warm = last_user_message_cached(&path, &mut cache, "s");
        assert!(
            warm.starts_with("huge pasted"),
            "the completed huge prompt must be surfaced, not permanently skipped (got {:?})",
            &warm[..warm.len().min(20)]
        );
    }

    #[test]
    fn cached_message_cold_rescans_after_rotation() {
        let dir = tempfile_dir();
        let path = dir.join("sess.jsonl");
        std::fs::write(&path, format!("{}\n", user_line("old prompt"))).unwrap();
        let mut cache: UserMessageCache = HashMap::new();
        assert_eq!(last_user_message_cached(&path, &mut cache, "s"), "old prompt");

        // Rotation/truncation: rewrite the file SHORTER with a different prompt.
        std::fs::write(&path, format!("{}\n", user_line("new"))).unwrap();
        assert_eq!(
            last_user_message_cached(&path, &mut cache, "s"),
            "new",
            "EOF < cached offset triggers a cold rescan, not a stale answer"
        );
    }
}
