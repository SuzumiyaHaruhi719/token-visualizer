//! Desktop side effects for the live session-poll loop.
//!
//! The poll + publish path (filesystem → `AppState` + SSE) lives in
//! [`cmserver::state_poll`] and is shared verbatim with `cm-serve` (browser
//! mode). This module supplies the DESKTOP-only reactions via
//! [`cmserver::StatePollHooks`]:
//! * **monitor toggle** → show/hide the tray popover,
//! * **session set changed** → refresh the tray tooltip,
//! * **session ended** → taskbar flash + toast + chime.
//!
//! All three touch Tauri windows, so they marshal to the main thread
//! (`AppHandle::run_on_main_thread`) where Windows window ops are safe — exactly
//! as the original inline loop did.

use cmcore::model::SessionState;
use cmserver::session_end::EndedSession;
use cmserver::{NotificationPrefs, StatePollHooks};
use tauri::AppHandle;

use crate::{notify, tray, windows};

/// Desktop implementation of [`StatePollHooks`]. Captures the [`AppHandle`] and
/// the server `port` so it can (re)show the popover against the running server.
pub struct DesktopHooks {
    app: AppHandle,
    port: u16,
}

impl DesktopHooks {
    /// Build the hooks for the desktop app.
    pub fn new(app: AppHandle, port: u16) -> Self {
        Self { app, port }
    }
}

impl StatePollHooks for DesktopHooks {
    /// The monitor toggle flipped (settings panel flips the atomic off the main
    /// thread); show/hide the popover here — on the main thread, where Windows
    /// window ops are safe. Turning it ON reveals the popover; OFF hides it.
    fn monitor_changed(&self, enabled: bool) {
        let app = self.app.clone();
        let port = self.port;
        let app_for_toggle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if enabled {
                windows::show_popover(&app_for_toggle, port);
            } else {
                windows::hide_popover(&app_for_toggle);
            }
        });
    }

    /// Refresh the tray tooltip from the most-recently-active session on the main
    /// thread (window ops must not run off the UI thread on Windows).
    fn sessions_changed(&self, sessions: &[SessionState]) {
        let app = self.app.clone();
        let current = sessions.first().cloned();
        let _ = app.clone().run_on_main_thread(move || {
            tray::update_tooltip(&app, current.as_ref());
        });
    }

    /// Fire taskbar-flash + toast + chime for each ended session on the main
    /// thread. The chime is gated on `prefs.sound_enabled` and played at
    /// `prefs.volume` (`0.0..=1.0`); the toast + flash on
    /// `prefs.notifications_enabled`.
    fn sessions_ended(&self, ended: &[EndedSession], prefs: NotificationPrefs) {
        let app = self.app.clone();
        let ended = ended.to_vec();
        let _ = app.clone().run_on_main_thread(move || {
            for session in &ended {
                notify::notify_session_ended(
                    &app,
                    session,
                    prefs.notifications_enabled,
                    prefs.sound_enabled,
                    prefs.volume,
                );
            }
        });
    }
}
