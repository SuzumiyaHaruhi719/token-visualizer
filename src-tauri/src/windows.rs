//! Window management: the dashboard window + one transparent always-on-top pet
//! window per active session.
//!
//! All windows are EXTERNAL webviews pointing at the embedded axum server
//! (`http://127.0.0.1:<port>/…`), so the same origin serves the dashboard, the
//! pet pages, and (if opened) a browser tab. Relative URLs in the frontend
//! therefore resolve against this server automatically.

use std::collections::HashSet;

use cmcore::model::SessionState;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::settings::{self, Settings};

/// The dashboard window label.
pub const DASHBOARD_LABEL: &str = "dashboard";

/// The tray "current session" popover window label.
pub const POPOVER_LABEL: &str = "popover";

/// Prefix for per-session pet window labels (`pet-<sessionId>`).
const PET_LABEL_PREFIX: &str = "pet-";

/// Popover (tray monitor) geometry. The popover stacks one card per active
/// session; height grows with the card count. These MUST match the popover.css
/// sizing contract: 6px outer padding, one 84px card per session, 8px gaps.
const POPOVER_W: f64 = 260.0;
const POPOVER_CARD_H: f64 = 84.0;
const POPOVER_CARD_GAP: f64 = 8.0;
const POPOVER_PAD: f64 = 6.0;
/// Max session cards rendered/sized at once (matches the frontend `MAX_CARDS`).
const POPOVER_MAX_CARDS: usize = 6;
/// Gap kept between the popover and the screen's bottom-right corner (logical px).
const POPOVER_MARGIN: f64 = 12.0;

/// Logical popover height for `n` session cards, clamped to `1..=MAX`.
/// `POPOVER_PAD + n*CARD_H + (n-1)*GAP + POPOVER_PAD`.
fn popover_height(n: usize) -> f64 {
    let n = n.clamp(1, POPOVER_MAX_CARDS) as f64;
    POPOVER_PAD + n * POPOVER_CARD_H + (n - 1.0) * POPOVER_CARD_GAP + POPOVER_PAD
}

/// Soft cap on simultaneous pet windows (design §12.5).
const MAX_PETS: usize = 8;

/// Pet window geometry. Height fits bubble + Clawd stage + optional tool tag +
/// project label without clipping (see `.pet-root` in `src/pet/pet.css`).
const PET_W: f64 = 220.0;
const PET_H: f64 = 270.0;
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
        // Vite dev server on a NON-1420 port (1420 is Tauri's default and would
        // collide with other local Tauri apps like CorePilot OSD).
        "http://localhost:5847".to_string()
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

/// Create the dashboard window **hidden** at startup. The app is tray-first, so
/// it never pops up or steals focus on launch/relaunch (which would cover
/// whatever you're working on). Reveal it on demand with [`show_dashboard`].
pub fn create_dashboard(app: &AppHandle, port: u16) -> tauri::Result<()> {
    if app.get_webview_window(DASHBOARD_LABEL).is_some() {
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
    .visible(false) // tray-first: do not show on launch
    .focused(false) // never grab foreground focus from whatever you're using
    .initialization_script(port_init_script(port))
    .build()?;
    Ok(())
}

/// Reveal + focus the dashboard (creating it if needed). Called from the tray
/// ("Open Dashboard" / tray click) — the only way the window ever appears.
pub fn show_dashboard(app: &AppHandle, port: u16) {
    if app.get_webview_window(DASHBOARD_LABEL).is_none() {
        let _ = create_dashboard(app, port);
    }
    if let Some(win) = app.get_webview_window(DASHBOARD_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Logical bottom-right anchor for a popover of the given `height` on the
/// primary monitor, leaving [`POPOVER_MARGIN`] from the right and bottom edges
/// (above the tray). Falls back to a sane default when no monitor info exists.
fn popover_position(app: &AppHandle, height: f64) -> (f64, f64) {
    if let Ok(Some(mon)) = app.primary_monitor() {
        let scale = mon.scale_factor();
        let size = mon.size();
        let pos = mon.position();
        // Convert the monitor's physical geometry to logical px (the unit used by
        // `WebviewWindowBuilder::position` / `inner_size`).
        let mon_x = pos.x as f64 / scale;
        let mon_y = pos.y as f64 / scale;
        let mon_w = size.width as f64 / scale;
        let mon_h = size.height as f64 / scale;
        let x = mon_x + mon_w - POPOVER_W - POPOVER_MARGIN;
        let y = mon_y + mon_h - height - POPOVER_MARGIN;
        return (x.max(mon_x), y.max(mon_y));
    }
    (POPOVER_MARGIN, POPOVER_MARGIN)
}

/// Create the popover window **hidden** at startup (like the dashboard). It is a
/// frameless, transparent, always-on-top, non-resizable card near the primary
/// monitor's bottom-right; revealed/hidden by [`toggle_popover`] (tray click).
pub fn create_popover(app: &AppHandle, port: u16) -> tauri::Result<()> {
    if app.get_webview_window(POPOVER_LABEL).is_some() {
        return Ok(());
    }
    let height = popover_height(1);
    let (x, y) = popover_position(app, height);
    WebviewWindowBuilder::new(
        app,
        POPOVER_LABEL,
        WebviewUrl::External(server_url(port, "/popover.html")),
    )
    .title("Current Session")
    .inner_size(POPOVER_W, height)
    .position(x, y)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .shadow(false)
    .visible(false) // tray-first: created hidden, shown on tray click
    .focused(false)
    .initialization_script(port_init_script(port))
    .build()?;
    Ok(())
}

/// Toggle the tray monitor popover: if it exists and is visible, save its
/// position and hide it; otherwise (re)create it, size it to `session_count`
/// active-session cards, restore the saved position (or anchor bottom-right),
/// show + focus. Called from the tray's LEFT-click handler.
pub fn toggle_popover(app: &AppHandle, port: u16, session_count: usize) {
    if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
        if win.is_visible().unwrap_or(false) {
            save_popover_position(&win);
            let _ = win.hide();
            return;
        }
        fit_popover(app, &win, session_count);
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    if create_popover(app, port).is_ok() {
        if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
            fit_popover(app, &win, session_count);
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// Live-resize the popover to `session_count` cards, but only while it is
/// already visible (so a new chat starting grows it without popping it open),
/// and only when the height actually changed (so per-tick state updates never
/// yank a window the user may be dragging). Called from the state-poll loop.
pub fn resize_popover_if_visible(app: &AppHandle, session_count: usize) {
    let Some(win) = app.get_webview_window(POPOVER_LABEL) else {
        return;
    };
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    let desired = popover_height(session_count);
    let scale = win.scale_factor().unwrap_or(1.0);
    if let Ok(sz) = win.inner_size() {
        let current = sz.height as f64 / scale;
        if (current - desired).abs() <= 1.0 {
            return; // unchanged — leave it (and the user's drag) alone
        }
    }
    fit_popover(app, &win, session_count);
}

/// Resize the popover to fit `session_count` cards and (re)position it.
fn fit_popover(app: &AppHandle, win: &WebviewWindow, session_count: usize) {
    let height = popover_height(session_count);
    let _ = win.set_size(tauri::LogicalSize::new(POPOVER_W, height));
    position_popover(app, win, height);
}

/// Hide the popover if it is currently visible. Saves the position first so
/// re-enabling restores it. Retained for the toggle-off path: the monitor toggle
/// now lives in the settings panel (it flips the shared atomic, which the tray
/// left-click already respects), so an open popover simply won't reopen — this
/// helper stays available for an immediate-hide wiring if desired later.
#[allow(dead_code)]
pub fn hide_popover(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
        if win.is_visible().unwrap_or(false) {
            save_popover_position(&win);
            let _ = win.hide();
        }
    }
}

/// Place the popover at the user's saved drag position if it is on a connected
/// monitor; otherwise anchor it to the primary monitor's bottom-right corner
/// using `height` (so a taller multi-session popover still sits above the tray).
fn position_popover(app: &AppHandle, win: &WebviewWindow, height: f64) {
    let saved = settings::load();
    if let (Some(x), Some(y)) = (saved.popover_x, saved.popover_y) {
        if position_on_some_monitor(app, x, y) {
            let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
            return;
        }
        // Saved spot is off-screen now (e.g. a monitor was unplugged) — fall
        // through to the safe bottom-right anchor so the popover is reachable.
    }
    let (x, y) = popover_position(app, height);
    let _ = win.set_position(tauri::LogicalPosition::new(x, y));
}

/// Persist the popover's current top-left (physical px) so the next open
/// restores where the user dragged it.
fn save_popover_position(win: &WebviewWindow) {
    if let Ok(pos) = win.outer_position() {
        let updated = Settings {
            popover_x: Some(pos.x as f64),
            popover_y: Some(pos.y as f64),
            ..settings::load()
        };
        settings::save(&updated);
    }
}

/// True if physical point `(x, y)` lies within any connected monitor's bounds.
fn position_on_some_monitor(app: &AppHandle, x: f64, y: f64) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return true; // can't tell — trust the saved value rather than fight it
    };
    monitors.iter().any(|m| {
        let p = m.position();
        let s = m.size();
        let (mx, my) = (p.x as f64, p.y as f64);
        x >= mx && y >= my && x < mx + s.width as f64 && y < my + s.height as f64
    })
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
            assert!(s.starts_with("http://localhost:5847/"), "dev base: {s}");
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

    #[test]
    fn popover_height_matches_card_contract() {
        // 6 pad + 1*84 + 0 gap + 6 pad
        assert_eq!(popover_height(1), 96.0);
        // 6 + 3*84 + 2*8 + 6 = 280
        assert_eq!(popover_height(3), 280.0);
        // 0 clamps up to 1 card; > MAX clamps down to MAX (6).
        assert_eq!(popover_height(0), popover_height(1));
        assert_eq!(popover_height(99), popover_height(POPOVER_MAX_CARDS));
    }
}
