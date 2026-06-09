//! Window management: the dashboard window + the tray "current session" popover.
//!
//! All windows are EXTERNAL webviews pointing at the embedded axum server
//! (`http://127.0.0.1:<port>/…`), so the same origin serves the dashboard, the
//! popover, and (if opened) a browser tab. Relative URLs in the frontend
//! therefore resolve against this server automatically.

use tauri::utils::config::WindowEffectsConfig;
use tauri::utils::WindowEffect;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use cmserver::settings;

/// The dashboard window label.
pub const DASHBOARD_LABEL: &str = "dashboard";

/// The tray "current session" popover window label.
pub const POPOVER_LABEL: &str = "popover";

/// Default popover (tray monitor) geometry. The popover is now RESIZABLE and
/// size-adaptive: as it grows it progressively reveals sections (token hero →
/// Codex session → top models → live sessions) and the hero reel's digit
/// precision adapts to the width. These are the INITIAL/default dimensions used
/// when the user hasn't resized it yet; the saved size (settings `popover_w/h`)
/// takes precedence on subsequent opens.
const POPOVER_W: f64 = 300.0;
/// Default height shows the token hero + the Codex session (with its 5h/weekly
/// limit bars) + today's top-3 models with no dead space (tier 3). On first
/// paint the frontend measures its real content and calls `set_popover_height`
/// to snap the window to an EXACT fit, so this is only the pre-measure default;
/// growing taller reveals the live-session list (tier 4). Kept loosely in sync
/// with the TS height breakpoints in `src/popover/popover-main.ts`.
const POPOVER_H: f64 = 392.0;
/// Minimum size the user can shrink the popover to (logical px). Keeps at least
/// the token hero comfortably visible.
const POPOVER_MIN_W: f64 = 224.0;
const POPOVER_MIN_H: f64 = 116.0;
/// Largest height the auto-fit / a saved size may grow the popover to (logical
/// px). A generous ceiling so a long live-session list still fits, while a freak
/// measurement can never produce a multi-thousand-pixel window.
const POPOVER_MAX_H: f64 = 900.0;
/// Gap kept between the popover and the screen's bottom-right corner (logical px).
const POPOVER_MARGIN: f64 = 12.0;

/// The popover's saved size, or the default when unset. Clamped to the minimum
/// so a corrupt/tiny saved value can never produce an unusable window.
fn popover_size() -> (f64, f64) {
    let s = settings::load();
    let w = s.popover_w.unwrap_or(POPOVER_W).clamp(POPOVER_MIN_W, POPOVER_MAX_H);
    let h = s.popover_h.unwrap_or(POPOVER_H).clamp(POPOVER_MIN_H, POPOVER_MAX_H);
    (w, h)
}

/// Clamp a frontend-measured popover height (logical px) to a safe target.
/// Returns `None` for garbage (NaN / ±inf / non-positive) so the caller bails
/// out without ever touching `set_size` — a runaway value like 65535 must never
/// balloon the window + hang the compositor (fix #1). Otherwise the value is
/// clamped to `[POPOVER_MIN_H, min(POPOVER_MAX_H, monitor_work_h)]`, where
/// `monitor_work_h` (when known) keeps the popover from ever exceeding the
/// height of the screen it lives on. Pure (no window) so it is unit-testable.
fn clamp_popover_height(height: f64, monitor_work_h: Option<f64>) -> Option<f64> {
    if !height.is_finite() || height <= 0.0 {
        return None;
    }
    let ceiling = monitor_work_h
        .filter(|h| h.is_finite() && *h > 0.0)
        .map(|h| h.min(POPOVER_MAX_H))
        .unwrap_or(POPOVER_MAX_H);
    let max_h = ceiling.max(POPOVER_MIN_H); // never invert the clamp range
    Some(height.clamp(POPOVER_MIN_H, max_h))
}

/// Resize the popover to a frontend-measured CONTENT height (logical px),
/// keeping the BOTTOM edge anchored so it grows UPWARD (a tray popover lives
/// above the taskbar — growing downward would slide it off-screen / under the
/// tray). Called over IPC from `popover-main.ts` after it measures its content,
/// so the window fits its content exactly with ZERO dead whitespace at every
/// tier. The height is clamped to `[POPOVER_MIN_H, min(POPOVER_MAX_H, monitor
/// work-area height)]` and garbage values are ignored; the width is preserved
/// (width is the user's to drag, and it drives the hero precision). The new size
/// is persisted so the next open restores the fitted height.
#[tauri::command]
pub fn set_popover_height(window: tauri::Window, height: f64) {
    let Some(win) = window.get_webview_window(POPOVER_LABEL) else {
        return;
    };
    let Ok(scale) = win.scale_factor() else { return };

    // Upper bound: the SMALLER of the static ceiling and the current monitor's
    // work-area height (logical px), so the popover can never grow taller than
    // the screen it lives on regardless of what the frontend measured.
    let monitor_work_h = win
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| m.size().height as f64 / m.scale_factor());
    let Some(target_h) = clamp_popover_height(height, monitor_work_h) else {
        return; // garbage measurement — ignore (never balloon the window)
    };

    // Current geometry in LOGICAL px (the unit `set_size`/`set_position` use).
    let Ok(outer) = win.outer_position() else { return };
    let Ok(size) = win.inner_size() else { return };
    let cur_w = size.width as f64 / scale;
    let cur_h = size.height as f64 / scale;
    let top = outer.y as f64 / scale;
    let left = outer.x as f64 / scale;

    // No-op when already within a hair of the target (avoids a resize feedback
    // loop with the frontend ResizeObserver, which would otherwise ping-pong).
    if (cur_h - target_h).abs() < 1.0 {
        return;
    }

    // Anchor the bottom: new top = old bottom - new height.
    let bottom = top + cur_h;
    let new_top = (bottom - target_h).max(0.0);

    let _ = win.set_size(tauri::LogicalSize::new(cur_w, target_h));
    let _ = win.set_position(tauri::LogicalPosition::new(left, new_top));

    // Persist the fitted height (+ position) so the next open restores it.
    save_popover_position(&win);
}

/// Build the acrylic window-effects config for the popover. On Windows 11 the
/// tint `color` is IGNORED (the system paints its own backdrop), so we pass
/// `None` and let the page paint a CSS tint for the live opacity slider. The
/// native acrylic backdrop is what avoids the old WebView2 transparent
/// "black box": the window is no longer empty-transparent — DWM composites a
/// blurred backdrop behind the webview.
fn popover_effects() -> WindowEffectsConfig {
    WindowEffectsConfig {
        effects: vec![WindowEffect::Acrylic],
        state: None,
        radius: None,
        color: None,
    }
}

/// Apply the acrylic effect to an existing popover window (best-effort). Used
/// after show so the backdrop is (re)asserted. If it fails (e.g. transparency
/// disabled system-wide), the page's CSS tint falls back to fully opaque so the
/// popover is still legible.
fn apply_popover_effects(win: &WebviewWindow) {
    if let Err(e) = win.set_effects(popover_effects()) {
        eprintln!("[popover] acrylic set_effects failed (using opaque CSS fallback): {e}");
    }
}

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
/// frameless, **transparent + acrylic**, always-on-top, **resizable** card near
/// the primary monitor's bottom-right; revealed/hidden by [`toggle_popover`]
/// (tray click).
///
/// Acrylic requires a transparent window. Unlike a *bare* transparent WebView2
/// window (which renders an ugly opaque "black box" around the content on
/// Windows — the reason this was previously `transparent(false)`), an acrylic
/// window has a NATIVE DWM backdrop composited behind the webview, so the
/// black-box artifact does not occur. The page paints a semi-transparent dark
/// tint over the blur (its alpha is the user's "Tray background" opacity).
pub fn create_popover(app: &AppHandle, port: u16) -> tauri::Result<()> {
    if app.get_webview_window(POPOVER_LABEL).is_some() {
        return Ok(());
    }
    let (w, h) = popover_size();
    let (x, y) = popover_position(app, h);
    WebviewWindowBuilder::new(
        app,
        POPOVER_LABEL,
        WebviewUrl::External(server_url(port, "/popover.html")),
    )
    .title("Today")
    .inner_size(w, h)
    .min_inner_size(POPOVER_MIN_W, POPOVER_MIN_H)
    .position(x, y)
    .resizable(true)
    .decorations(false)
    // Transparent + acrylic native backdrop (see fn doc). The page paints its
    // own dark tint OVER the blur; the OS rounds the frameless window.
    .transparent(true)
    .effects(popover_effects())
    .always_on_top(true)
    .skip_taskbar(true)
    // No OS shadow: on an undecorated transparent Windows window it can add a
    // 1px light border around the rounded acrylic. The blur + tint read as a
    // floating card on their own.
    .shadow(false)
    .visible(false) // tray-first: created hidden, shown on tray click
    .focused(false)
    .initialization_script(port_init_script(port))
    .build()?;
    // Round the NATIVE window corners so the acrylic backdrop matches the
    // rounded card (otherwise DWM paints a hard square block of blur behind the
    // rounded `#popover`). Best-effort, Windows 11 only.
    if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
        round_window_corners(&win);
    }
    Ok(())
}

/// Apply DWM rounded corners to a window (Windows 11). No-op / harmless on
/// other platforms + older Windows. Pairs with the acrylic backdrop so the
/// blurred region is rounded, not a square behind the card.
#[cfg(windows)]
fn round_window_corners(win: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        DWM_WINDOW_CORNER_PREFERENCE,
    };
    let Ok(handle) = win.hwnd() else { return };
    let hwnd = HWND(handle.0 as _);
    let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    // SAFETY: hwnd is a live top-level window owned by this process; the
    // attribute + size match the DWM contract. Errors are ignored (older OS).
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn round_window_corners(_win: &WebviewWindow) {}

/// Toggle the tray monitor popover: if it exists and is visible, save its
/// position and hide it; otherwise (re)create it, restore the saved position (or
/// anchor bottom-right), show + focus. Called from the tray's LEFT-click handler.
pub fn toggle_popover(app: &AppHandle, port: u16) {
    if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
        if win.is_visible().unwrap_or(false) {
            save_popover_position(&win);
            let _ = win.hide();
            return;
        }
        fit_popover(app, &win);
        let _ = win.show();
        let _ = win.set_always_on_top(true); // re-assert so it stays on top
        let _ = win.set_focus();
        return;
    }
    if create_popover(app, port).is_ok() {
        if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
            fit_popover(app, &win);
            let _ = win.show();
            let _ = win.set_always_on_top(true);
            let _ = win.set_focus();
        }
    }
}

/// Restore the popover's SAVED size (or the default) and (re)position it, then
/// re-assert the acrylic backdrop. The popover is resizable now, so we no longer
/// force a single fixed size — we honor whatever the user last dragged it to.
fn fit_popover(app: &AppHandle, win: &WebviewWindow) {
    let (w, h) = popover_size();
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    position_popover(app, win, h);
    apply_popover_effects(win);
    // Re-assert DWM rounded corners: a resize/show can reset the corner
    // preference on some Windows builds, which would bring back the square
    // acrylic block behind the rounded card. Cheap + idempotent.
    round_window_corners(win);
}

/// Show the popover (creating it if missing) at a guaranteed-visible position.
/// Called at startup when the monitor is enabled and when the monitor is
/// switched ON from the settings panel (mirrors [`hide_popover`]).
pub fn show_popover(app: &AppHandle, port: u16) {
    if app.get_webview_window(POPOVER_LABEL).is_none() {
        let _ = create_popover(app, port);
    }
    if let Some(win) = app.get_webview_window(POPOVER_LABEL) {
        fit_popover(app, &win);
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_always_on_top(true); // re-assert so it stays on top
        let _ = win.set_focus();
    }
}

/// Hide the popover if it is currently visible. Saves the position first so
/// re-enabling restores it. Called by the state-poll loop when the monitor is
/// switched off in the settings panel (the API flips the shared atomic; the
/// poll loop performs the actual hide on the main thread).
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

/// Persist the popover's current top-left (physical px) AND size (logical px) so
/// the next open restores where the user dragged it and how big they made it.
fn save_popover_position(win: &WebviewWindow) {
    let base = settings::load();
    let mut updated = base.clone();
    if let Ok(pos) = win.outer_position() {
        updated.popover_x = Some(pos.x as f64);
        updated.popover_y = Some(pos.y as f64);
    }
    // Inner size in LOGICAL px (matches how we set it). Clamp to the minimum so
    // a freak value never persists an unusable size.
    if let Ok(size) = win.inner_size() {
        if let Ok(scale) = win.scale_factor() {
            let w = (size.width as f64 / scale).max(POPOVER_MIN_W);
            let h = (size.height as f64 / scale).max(POPOVER_MIN_H);
            updated.popover_w = Some(w);
            updated.popover_h = Some(h);
        }
    }
    settings::save(&updated);
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
    fn popover_defaults_exceed_minimums() {
        // The popover is now resizable + size-adaptive. The default size must be
        // at least the minimum the user can shrink to, and the minimum must be
        // a sane positive floor. The default must also sit within the auto-fit
        // ceiling so a fresh open never opens larger than the max.
        assert!(POPOVER_W >= POPOVER_MIN_W);
        assert!(POPOVER_H >= POPOVER_MIN_H);
        assert!(POPOVER_MIN_W > 0.0 && POPOVER_MIN_H > 0.0);
        assert!(POPOVER_MAX_H > POPOVER_MIN_H);
        assert!(POPOVER_H <= POPOVER_MAX_H);
    }

    #[test]
    fn clamp_popover_height_rejects_garbage() {
        // NaN / inf / zero / negative must yield None so the command bails out
        // and never feeds a runaway value into set_size (the 65535 freeze).
        assert!(clamp_popover_height(f64::NAN, None).is_none());
        assert!(clamp_popover_height(f64::INFINITY, None).is_none());
        assert!(clamp_popover_height(f64::NEG_INFINITY, None).is_none());
        assert!(clamp_popover_height(0.0, None).is_none());
        assert!(clamp_popover_height(-50.0, None).is_none());
    }

    #[test]
    fn clamp_popover_height_clamps_to_min_and_max() {
        // Below the floor → MIN; above the static ceiling (no monitor) → MAX.
        assert_eq!(clamp_popover_height(10.0, None), Some(POPOVER_MIN_H));
        assert_eq!(clamp_popover_height(65535.0, None), Some(POPOVER_MAX_H));
        // A sane value passes through unchanged.
        assert_eq!(clamp_popover_height(400.0, None), Some(400.0));
    }

    #[test]
    fn clamp_popover_height_respects_monitor_work_area() {
        // A short monitor caps the height below the static ceiling, so the
        // popover can never be taller than the screen it lives on.
        let mon = Some(600.0);
        assert_eq!(clamp_popover_height(65535.0, mon), Some(600.0));
        assert_eq!(clamp_popover_height(500.0, mon), Some(500.0));
        // A garbage monitor height is ignored (falls back to the static ceiling).
        assert_eq!(clamp_popover_height(65535.0, Some(f64::NAN)), Some(POPOVER_MAX_H));
        assert_eq!(clamp_popover_height(65535.0, Some(0.0)), Some(POPOVER_MAX_H));
        // A monitor shorter than the min floor can't invert the clamp range.
        assert_eq!(clamp_popover_height(500.0, Some(50.0)), Some(POPOVER_MIN_H));
    }

    #[test]
    fn popover_effects_request_acrylic_without_native_tint() {
        // Acrylic is the chosen native backdrop; the tint color is left None
        // because Windows 11 ignores it (the live opacity is a CSS layer).
        let cfg = popover_effects();
        assert!(cfg.effects.contains(&WindowEffect::Acrylic));
        assert!(cfg.color.is_none());
    }
}
