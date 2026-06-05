//! Window management: the dashboard window + one transparent always-on-top pet
//! window per active session.
//!
//! All windows are EXTERNAL webviews pointing at the embedded axum server
//! (`http://127.0.0.1:<port>/…`), so the same origin serves the dashboard, the
//! pet pages, and (if opened) a browser tab. Relative URLs in the frontend
//! therefore resolve against this server automatically.

use std::collections::HashSet;

use cmcore::model::SessionState;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The dashboard window label.
pub const DASHBOARD_LABEL: &str = "dashboard";

/// Prefix for per-session pet window labels (`pet-<sessionId>`).
const PET_LABEL_PREFIX: &str = "pet-";

/// Soft cap on simultaneous pet windows (design §12.5).
const MAX_PETS: usize = 8;

/// Pet window geometry.
const PET_W: f64 = 220.0;
const PET_H: f64 = 200.0;
/// Cascade offsets between successive pet windows.
const PET_DX: f64 = 60.0;
const PET_DY: f64 = 40.0;
/// First pet anchor (top-left-ish of the primary screen).
const PET_X0: f64 = 80.0;
const PET_Y0: f64 = 80.0;

/// Build the URL for a server-served page (relative path under the origin).
fn server_url(port: u16, path: &str) -> tauri::Url {
    format!("http://127.0.0.1:{port}{path}")
        .parse()
        .expect("static server URL is always valid")
}

/// Create (or focus) the dashboard window pointing at the server root.
pub fn create_dashboard(app: &AppHandle, port: u16) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window(DASHBOARD_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        DASHBOARD_LABEL,
        WebviewUrl::External(server_url(port, "/")),
    )
    .title("Claude Monitor")
    .inner_size(1100.0, 720.0)
    .resizable(true)
    .build()?;
    Ok(())
}

/// Show + focus the dashboard window if it exists, else (re)create it.
pub fn show_dashboard(app: &AppHandle, port: u16) {
    if let Some(win) = app.get_webview_window(DASHBOARD_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        let _ = create_dashboard(app, port);
    }
}

/// Reconcile pet windows against the current active-session list:
/// spawn a window for each new session, close windows whose session vanished.
///
/// Honors a soft cap of [`MAX_PETS`]; extra sessions are ignored (no spam).
pub fn sync_pets(app: &AppHandle, port: u16, sessions: &[SessionState]) {
    // Desired set of session ids (capped).
    let desired: Vec<&SessionState> = sessions.iter().take(MAX_PETS).collect();
    let desired_ids: HashSet<String> = desired.iter().map(|s| s.session_id.clone()).collect();

    // 1. Close pet windows whose session disappeared.
    for (label, win) in app.webview_windows() {
        if let Some(sid) = label.strip_prefix(PET_LABEL_PREFIX) {
            if !desired_ids.contains(sid) {
                // A short delay would let the frontend play a leave animation;
                // the frontend drives that on its own when the session drops
                // from the SSE feed, so closing here is sufficient and simple.
                let _ = win.close();
            }
        }
    }

    // 2. Spawn windows for new sessions (cascade their positions).
    for (idx, session) in desired.iter().enumerate() {
        let label = format!("{PET_LABEL_PREFIX}{}", session.session_id);
        if app.get_webview_window(&label).is_some() {
            continue; // already open
        }
        let x = PET_X0 + (idx as f64) * PET_DX;
        let y = PET_Y0 + (idx as f64) * PET_DY;
        let path = format!("/pet.html?session={}", encode_session(&session.session_id));
        let res = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(server_url(port, &path)))
            .title(&session.project)
            .inner_size(PET_W, PET_H)
            .position(x, y)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .shadow(false)
            .build();
        if let Err(e) = res {
            eprintln!("[windows] failed to spawn pet {label}: {e}");
        }
    }
}

/// Minimal URL-encoding for a session id placed in a query string. Session ids
/// are UUID-shaped (already URL-safe), but encode defensively anyway.
fn encode_session(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_builds_relative_paths() {
        let u = server_url(54321, "/api/summary?range=all");
        assert_eq!(u.as_str(), "http://127.0.0.1:54321/api/summary?range=all");
    }

    #[test]
    fn encode_session_passes_uuid_through() {
        assert_eq!(
            encode_session("639e6a3d-23bb-4d25-a9f0-43ecced997f1"),
            "639e6a3d-23bb-4d25-a9f0-43ecced997f1"
        );
    }

    #[test]
    fn encode_session_escapes_unsafe() {
        assert_eq!(encode_session("a b/c"), "a%20b%2Fc");
    }
}
