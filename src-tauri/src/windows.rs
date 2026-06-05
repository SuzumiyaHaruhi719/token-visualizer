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

/// URL to LOAD a frontend page. In debug builds the page is served by the Vite
/// dev server (so frontend edits hot-reload via `tauri dev`); in release it is
/// served by the embedded axum server. Either way the page talks to the axum
/// API through the injected `window.__CM_PORT__` (see [`port_init_script`]).
fn server_url(port: u16, path: &str) -> tauri::Url {
    let base = if cfg!(debug_assertions) {
        // Vite dev server (tauri.conf `devUrl` / `beforeDevCommand: npm run dev`).
        "http://localhost:1420".to_string()
    } else {
        format!("http://127.0.0.1:{port}")
    };
    format!("{base}{path}")
        .parse()
        .expect("page URL is always valid")
}

/// Script injected before page load so the frontend's API client targets the
/// axum server even when the page itself is served by the Vite dev server.
fn port_init_script(port: u16) -> String {
    format!("window.__CM_PORT__ = {port};")
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
    .initialization_script(port_init_script(port))
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

/// Close every open pet window (labels prefixed with [`PET_LABEL_PREFIX`]).
/// Used when desktop pets are toggled off.
pub fn close_all_pets(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.strip_prefix(PET_LABEL_PREFIX).is_some() {
            let _ = win.close();
        }
    }
}

/// Reconcile pet windows against the current active-session list:
/// spawn a window for each new session, close windows whose session vanished.
///
/// Honors a soft cap of [`MAX_PETS`]; extra sessions are ignored (no spam).
/// When `pets_enabled` is false, all pet windows are closed and nothing spawns.
pub fn sync_pets(app: &AppHandle, port: u16, sessions: &[SessionState], pets_enabled: bool) {
    if !pets_enabled {
        close_all_pets(app);
        return;
    }
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
            .initialization_script(port_init_script(port))
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
    fn server_url_builds_pages_per_profile() {
        let u = server_url(54321, "/api/summary?range=all");
        let s = u.as_str();
        assert!(s.ends_with("/api/summary?range=all"), "path preserved: {s}");
        if cfg!(debug_assertions) {
            // dev: page served by the Vite dev server (HMR)
            assert!(s.starts_with("http://localhost:1420/"), "dev base: {s}");
        } else {
            // release: page served by the embedded axum server
            assert!(s.starts_with("http://127.0.0.1:54321/"), "release base: {s}");
        }
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
