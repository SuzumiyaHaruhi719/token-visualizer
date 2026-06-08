//! Session-end detection + notifications (taskbar flash + Windows toast + chime).
//!
//! The state-poll loop emits the full list of live [`SessionState`]s each tick.
//! This module tracks which session ids were live on the previous tick and,
//! when an id drops out, treats that session as ENDED. Each ended session
//! fires three independent, best-effort side effects:
//!
//! 1. **Taskbar flash** — `request_user_attention` on the dashboard window so the
//!    taskbar button flashes even while the window is hidden.
//! 2. **Windows toast** — via `tauri-plugin-notification`.
//! 3. **Chime** — the bundled `assets/session-end.wav`, played async through the
//!    Win32 `PlaySoundW` API (no audio-graph deps).
//!
//! The detection itself ([`SessionEndTracker`]) is pure and unit-tested; the
//! side effects live behind [`notify_session_ended`] and must run on the main
//! thread (Windows window ops are not thread-safe off the UI thread).

use std::collections::HashMap;
use std::collections::HashSet;

use cmcore::model::SessionState;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::windows::DASHBOARD_LABEL;

/// A session that has just ended, captured at the moment it dropped out of the
/// live set. Carries enough context to build a friendly toast body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedSession {
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub tokens: i64,
}

impl From<&SessionState> for EndedSession {
    fn from(s: &SessionState) -> Self {
        Self {
            session_id: s.session_id.clone(),
            project: s.project.clone(),
            model: s.model.clone(),
            tokens: s.tokens,
        }
    }
}

/// Tracks the live-session set across ticks to detect ends.
///
/// The FIRST observed tick only seeds the baseline (so sessions already running
/// when the app launched do not all immediately toast). Every subsequent tick
/// diffs the previous live set against the current one and reports the sessions
/// that disappeared.
#[derive(Debug, Default)]
pub struct SessionEndTracker {
    /// Full snapshot of the previous tick's live sessions, keyed by id. Kept
    /// (not just the id set) so an ended session can still produce a rich toast
    /// (project / model / tokens) from its last-known state.
    prev_live: HashMap<String, SessionState>,
    /// Whether at least one tick has been observed (baseline established).
    seeded: bool,
}

impl SessionEndTracker {
    /// Create an empty tracker (baseline not yet seeded).
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current live sessions and return those that just ENDED.
    ///
    /// On the first call this only seeds the baseline and returns an empty Vec.
    /// Afterwards it returns one [`EndedSession`] per id present last tick but
    /// absent now (multiple concurrent ends are all reported).
    pub fn observe(&mut self, current: &[SessionState]) -> Vec<EndedSession> {
        let ended = ended_sessions(&self.prev_live, current, self.seeded);
        self.prev_live = current
            .iter()
            .map(|s| (s.session_id.clone(), s.clone()))
            .collect();
        self.seeded = true;
        ended
    }
}

/// Pure diff: given the previous tick's live sessions and the current ones,
/// return the sessions that have ended (present before, absent now). When
/// `seeded` is false this is the baseline pass and nothing is reported.
fn ended_sessions(
    prev_live: &HashMap<String, SessionState>,
    current: &[SessionState],
    seeded: bool,
) -> Vec<EndedSession> {
    if !seeded {
        return Vec::new();
    }
    let current_ids: HashSet<&str> = current.iter().map(|s| s.session_id.as_str()).collect();
    prev_live
        .iter()
        .filter(|(id, _)| !current_ids.contains(id.as_str()))
        .map(|(_, prev)| EndedSession::from(prev))
        .collect()
}

/// Fire all three side effects for one ended session. Best-effort: every step is
/// independent and a failure in one never blocks the others (nor panics).
///
/// MUST be called on the main thread (window ops). The caller marshals via
/// `AppHandle::run_on_main_thread`.
pub fn notify_session_ended(app: &AppHandle, ended: &EndedSession) {
    flash_taskbar(app);
    send_toast(app, ended);
    play_chime(app);
}

/// Flash the dashboard window's taskbar button to draw attention. If the window
/// does not exist yet, there is nothing to flash — skip silently.
fn flash_taskbar(app: &AppHandle) {
    let Some(win) = app.get_webview_window(DASHBOARD_LABEL) else {
        return;
    };
    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
}

/// Send a Windows toast describing the ended session. Body format:
/// `"<project> · <model> · <N> tokens"`, falling back to a generic label when
/// the project/model are unknown (they are, once the session has dropped out).
fn send_toast(app: &AppHandle, ended: &EndedSession) {
    let body = toast_body(ended);
    if let Err(e) = app
        .notification()
        .builder()
        .title("Claude session finished")
        .body(&body)
        .show()
    {
        eprintln!("[notify] toast failed: {e}");
    }
}

/// Build the toast body. Because an ended session has usually lost its rich
/// context, this degrades gracefully to "A session has ended." when empty.
fn toast_body(ended: &EndedSession) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !ended.project.is_empty() {
        parts.push(ended.project.clone());
    }
    if !ended.model.is_empty() {
        parts.push(ended.model.clone());
    }
    if ended.tokens > 0 {
        parts.push(format!(
            "{} tokens",
            crate::util::format_thousands(ended.tokens)
        ));
    }
    if parts.is_empty() {
        "A session has ended.".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Resolve the bundled chime path and play it asynchronously (fire-and-forget).
/// In a bundled app the WAV lives in the Tauri resource dir; in a `cargo build`
/// dev run it falls back to `CARGO_MANIFEST_DIR/assets`. A missing file is
/// logged and skipped — never a panic.
fn play_chime(app: &AppHandle) {
    let Some(path) = chime_path(app) else {
        eprintln!("[notify] chime wav not found; skipping sound");
        return;
    };
    #[cfg(windows)]
    play_wav_async(&path);
    #[cfg(not(windows))]
    let _ = path; // non-Windows builds (tests/CI) are silent by design.
}

/// Locate `assets/session-end.wav`: prefer the Tauri resource dir (production
/// bundle), fall back to the crate's source `assets/` for non-bundled dev runs.
fn chime_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    const REL: &str = "assets/session-end.wav";
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join(REL);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(REL);
    if dev.is_file() {
        return Some(dev);
    }
    None
}

/// Play a WAV file via Win32 `PlaySoundW` with `SND_FILENAME | SND_ASYNC`:
/// returns immediately, the OS handles playback. Errors are non-fatal.
#[cfg(windows)]
fn play_wav_async(path: &std::path::Path) {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};

    // Build a wide, null-terminated path string that must outlive the call.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is a valid null-terminated UTF-16 buffer alive for the
    // duration of the call; SND_ASYNC copies what it needs and returns at once.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(wide.as_ptr()),
            None,
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !ok.as_bool() {
        eprintln!("[notify] PlaySoundW returned false for {}", path.display());
    }
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;
    use cmcore::model::PetState;

    fn session(id: &str) -> SessionState {
        SessionState {
            session_id: id.to_string(),
            project: "proj".to_string(),
            model: "claude-opus-4-8".to_string(),
            state: PetState::Idle,
            tokens: 1234,
            updated_at: 0,
        }
    }

    #[test]
    fn baseline_tick_fires_nothing() {
        let mut tracker = SessionEndTracker::new();
        let ended = tracker.observe(&[session("a"), session("b")]);
        assert!(ended.is_empty(), "first tick only seeds the baseline");
    }

    #[test]
    fn disappearing_session_fires() {
        let mut tracker = SessionEndTracker::new();
        tracker.observe(&[session("a"), session("b")]); // baseline
        let ended = tracker.observe(&[session("a")]); // b ended
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].session_id, "b");
    }

    #[test]
    fn new_session_does_not_fire() {
        let mut tracker = SessionEndTracker::new();
        tracker.observe(&[session("a")]); // baseline
        let ended = tracker.observe(&[session("a"), session("c")]); // c appeared
        assert!(ended.is_empty());
    }

    #[test]
    fn concurrent_ends_all_fire() {
        let mut tracker = SessionEndTracker::new();
        tracker.observe(&[session("a"), session("b"), session("c")]);
        let mut ended: Vec<String> =
            tracker.observe(&[]).into_iter().map(|e| e.session_id).collect();
        ended.sort();
        assert_eq!(ended, vec!["a", "b", "c"]);
    }

    #[test]
    fn restart_after_disappear_is_end_then_new() {
        let mut tracker = SessionEndTracker::new();
        tracker.observe(&[session("a")]); // baseline
        let ended = tracker.observe(&[]); // a ended
        assert_eq!(ended.len(), 1);
        let ended2 = tracker.observe(&[session("a")]); // a reappears: no end
        assert!(ended2.is_empty());
    }

    #[test]
    fn toast_body_degrades_when_context_missing() {
        let bare = EndedSession {
            session_id: "x".into(),
            project: String::new(),
            model: String::new(),
            tokens: 0,
        };
        assert_eq!(toast_body(&bare), "A session has ended.");
    }

    #[test]
    fn toast_body_joins_known_fields() {
        let rich = EndedSession {
            session_id: "x".into(),
            project: "claude-monitor".into(),
            model: "claude-opus-4-8".into(),
            tokens: 12_345,
        };
        assert_eq!(
            toast_body(&rich),
            "claude-monitor · claude-opus-4-8 · 12,345 tokens"
        );
    }
}
