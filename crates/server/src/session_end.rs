//! Session-end detection (pure) — shared by the headless poll loop and the
//! desktop notification layer.
//!
//! The state-poll loop emits the full list of live [`SessionState`]s each tick.
//! [`SessionEndTracker`] tracks which session ids were live on the previous tick
//! and, when an id drops out, treats that session as ENDED. The detection is
//! pure (no GUI), so it lives here in `cmserver`; the desktop side effects
//! (taskbar flash + toast + chime) stay in the Tauri crate and consume the
//! [`EndedSession`] list this produces.

use std::collections::HashMap;
use std::collections::HashSet;

use cmcore::model::SessionState;

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

/// Build the toast body for an ended session. Because an ended session has
/// usually lost its rich context, this degrades gracefully to "A session has
/// ended." when empty. Pure + unit-tested; used by the desktop toast layer.
pub fn toast_body(ended: &EndedSession) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !ended.project.is_empty() {
        parts.push(ended.project.clone());
    }
    if !ended.model.is_empty() {
        parts.push(ended.model.clone());
    }
    if ended.tokens > 0 {
        parts.push(format!("{} tokens", crate::util::format_thousands(ended.tokens)));
    }
    if parts.is_empty() {
        "A session has ended.".to_string()
    } else {
        parts.join(" · ")
    }
}

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
            last_user_message: String::new(),
            source: cmcore::model::Source::Claude,
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
