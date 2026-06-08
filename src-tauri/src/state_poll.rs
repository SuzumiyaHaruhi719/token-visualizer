//! Periodic derivation of live session/pet state from `~/.claude` (read-only).
//!
//! Every [`POLL_INTERVAL`] this loop:
//! 1. enumerates `sessions/*.json` for coarse busy/idle status + heartbeats,
//! 2. maps each session to its jsonl (`<session_id>.jsonl` under `projects/**`)
//!    and reads the tail for fine-grained [`cmcore::model::LineKind`] signals,
//! 3. derives a [`SessionState`] per session via [`cmcore::state`],
//! 4. when the set changed, updates [`AppState`] and broadcasts `sessions`/`usage`.
//!
//! When `sessions/` is empty or missing, it falls back to treating
//! recently-modified jsonl files as the active sessions (so pets still appear
//! for live work even before a status file exists). Strictly read-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cmcore::importer::read_new_complete_lines;
use cmcore::model::{LineKind, SessionState};
use cmcore::state::{self, SessionStatus};
use cmcore::store::Store;
use tauri::AppHandle;
use walkdir::WalkDir;

use crate::notify::{self, SessionEndTracker};
use crate::server::{AppState, SseEvent};
use crate::{tray, windows};

/// How often the loop recomputes session state.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// How many trailing bytes of a session jsonl to scan for recent line kinds.
const TAIL_BYTES: u64 = 64 * 1024;

/// A jsonl modified within this window (no status file) is treated as active.
const ACTIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// Run the poll loop forever. Spawn this on a dedicated OS thread; it opens its
/// own read `Store` per pass (never shares one across threads).
///
/// `app` drives the desktop side effects (pet windows + tray tooltip), executed
/// on the main thread. The broadcast + shared state are updated regardless.
pub fn run(
    state: AppState,
    app: AppHandle,
    port: u16,
    pets_enabled: Arc<AtomicBool>,
    session_count: Arc<AtomicUsize>,
) {
    // Cache of session_id -> jsonl path so we don't re-walk the whole tree
    // every second. Paths are stable once discovered.
    let mut path_cache: HashMap<String, PathBuf> = HashMap::new();
    let mut last_emitted: Vec<SessionState> = Vec::new();
    let mut last_enabled = pets_enabled.load(Ordering::Relaxed);
    // Tracks the live-session set across ticks to fire session-end notifications.
    let mut end_tracker = SessionEndTracker::new();

    loop {
        let enabled = pets_enabled.load(Ordering::Relaxed);
        let enabled_changed = enabled != last_enabled;
        last_enabled = enabled;

        let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match poll_once(&state, &mut path_cache) {
                Ok(sessions) => {
                    // Session-end detection runs EVERY tick (even when the
                    // emitted set is unchanged) so a disappearing session is
                    // never missed by the change short-circuit below.
                    let ended = end_tracker.observe(&sessions);
                    if !ended.is_empty() {
                        notify_ended(&app, ended);
                    }

                    // Publish the live count every tick so the tray click can
                    // size the monitor popover to the active-session count.
                    session_count.store(sessions.len(), Ordering::Relaxed);

                    if sessions != last_emitted || enabled_changed {
                        last_emitted = sessions.clone();
                        publish(&state, sessions.clone());
                        drive_desktop(&app, port, sessions, enabled);
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

/// Fire taskbar-flash + toast + chime for each ended session on the main thread
/// (Windows window ops must not run off the UI thread).
fn notify_ended(app: &AppHandle, ended: Vec<notify::EndedSession>) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        for session in &ended {
            notify::notify_session_ended(&app, session);
        }
    });
}

/// Reconcile pet windows + tray tooltip on the main thread (window ops must not
/// run off the UI thread on Windows).
fn drive_desktop(app: &AppHandle, port: u16, sessions: Vec<SessionState>, pets_enabled: bool) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let current = sessions.first().cloned();
        windows::sync_pets(&app, port, &sessions, pets_enabled);
        // Grow/shrink an OPEN monitor popover to match the live session count.
        windows::resize_popover_if_visible(&app, sessions.len());
        tray::update_tooltip(&app, current.as_ref());
    });
}

/// One polling pass. Pure-ish: reads the filesystem + store, returns the derived
/// session list (most-recently-updated first).
fn poll_once(
    state: &AppState,
    path_cache: &mut HashMap<String, PathBuf>,
) -> anyhow::Result<Vec<SessionState>> {
    let now_ms = now_ms();
    let store = Store::open(&state.db_path)?;

    let statuses = read_session_statuses();
    let mut sessions: Vec<SessionState> = if statuses.is_empty() {
        // Fallback: derive active sessions from recently-touched jsonl files.
        derive_from_recent_jsonl(&store, now_ms)?
    } else {
        derive_from_status_files(&store, &statuses, path_cache, now_ms)?
    };

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
    now_ms: i64,
) -> anyhow::Result<Vec<SessionState>> {
    let mut out = Vec::new();
    for st in statuses {
        let Some(session_id) = st.session_id.clone() else {
            continue;
        };
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
        let running_tokens = cmcore::query::session_tokens(store, &session_id).unwrap_or(0);

        let session = state::session_state_from(
            &session_id,
            &project,
            &model,
            &st.status,
            &recent_lines,
            running_tokens,
            now_ms,
            last_activity,
        );
        out.push(session);
    }
    Ok(out)
}

/// Fallback path: when no status files exist, treat any jsonl modified within
/// [`ACTIVE_WINDOW_MS`] as an active session and derive purely from its tail.
fn derive_from_recent_jsonl(store: &Store, now_ms: i64) -> anyhow::Result<Vec<SessionState>> {
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
        let (project, model, last_activity_ms, recent_lines) = read_tail_signals(&path, now_ms);
        let running_tokens = cmcore::query::session_tokens(store, &session_id).unwrap_or(0);

        // No status file -> empty status string -> core falls back to line-based
        // derivation (with idle/sleep staleness still applied).
        let session = state::session_state_from(
            &session_id,
            &project,
            &model,
            "",
            &recent_lines,
            running_tokens,
            now_ms,
            last_activity_ms,
        );
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

    // Project + model + last activity from the most recent assistant event.
    let mut project = String::new();
    let mut model = String::new();
    let mut last_activity = fallback_ms;
    for ev in read.events.iter().rev() {
        if project.is_empty() {
            project = ev.project.clone();
        }
        if model.is_empty() {
            model = ev.model.clone();
        }
        last_activity = last_activity.max(ev.ts);
        if !project.is_empty() && !model.is_empty() {
            break;
        }
    }
    if project.is_empty() {
        // Decode the jsonl directory name (e.g. C--Users-...-CorePilot).
        project = project_from_dir(path);
    }
    if last_activity == fallback_ms {
        // No assistant events in the tail: use the file mtime as activity.
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
        let dir = std::env::temp_dir().join(format!(
            "cm-state-poll-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
