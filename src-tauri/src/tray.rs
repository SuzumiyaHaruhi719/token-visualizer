//! System tray icon + menu.
//!
//! Menu: "Open Dashboard" (shows/focuses the dashboard window) and "Quit"
//! (exits the app). The pets + monitor toggles now live in the dashboard's
//! settings panel, not here. The tooltip reflects the most-recently-active
//! session and is refreshed best-effort from the state-poll loop via
//! [`update_tooltip`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cmcore::model::SessionState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::AppHandle;

use crate::windows;

/// Menu item id: show the dashboard.
const ID_OPEN: &str = "open_dashboard";
/// Menu item id: quit the app.
const ID_QUIT: &str = "quit";

/// The fixed tray id so we can look the icon back up to update its tooltip.
pub const TRAY_ID: &str = "cm-tray";

/// Build the tray icon with its menu. `port` is captured so "Open Dashboard"
/// can (re)create the dashboard window against the running server.
/// `monitor_enabled` gates the left-click popover (toggled from the settings
/// panel).
pub fn build(
    app: &AppHandle,
    port: u16,
    monitor_enabled: Arc<AtomicBool>,
) -> tauri::Result<TrayIcon> {
    let open_item = MenuItem::with_id(app, ID_OPEN, "Open Dashboard", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, ID_QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Claude Monitor")
        .on_menu_event(move |app, event| match event.id.as_ref() {
            ID_OPEN => windows::show_dashboard(app, port),
            ID_QUIT => app.exit(0),
            _ => {}
        })
        // LEFT-click (button release) toggles the current-session popover, but
        // only when the monitor is enabled. The context menu still opens on
        // RIGHT-click (`show_menu_on_left_click` stays false), so left vs. right
        // are distinct gestures.
        .on_tray_icon_event(move |tray, event| {
            if matches!(
                event,
                tauri::tray::TrayIconEvent::Click {
                    button: tauri::tray::MouseButton::Left,
                    button_state: tauri::tray::MouseButtonState::Up,
                    ..
                }
            ) && monitor_enabled.load(Ordering::Relaxed)
            {
                windows::toggle_popover(tray.app_handle(), port);
            }
        });

    // Reuse the app's window icon for the tray when available.
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)
}

/// Best-effort tooltip refresh from the current session. Called from the
/// state-poll loop; failures are ignored (the tray is non-critical).
pub fn update_tooltip(app: &AppHandle, current: Option<&SessionState>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let text = match current {
        Some(s) => format!(
            "Claude Monitor — {} · {} · {} tok",
            s.project,
            s.model,
            format_tokens(s.tokens)
        ),
        None => "Claude Monitor — idle".to_string(),
    };
    let _ = tray.set_tooltip(Some(text));
}

/// Compact token formatting for the tooltip (e.g. `1.2M`, `34.0K`).
fn format_tokens(n: i64) -> String {
    let f = n as f64;
    if f >= 1_000_000.0 {
        format!("{:.1}M", f / 1_000_000.0)
    } else if f >= 1_000.0 {
        format!("{:.1}K", f / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tokens_scales() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(34_000), "34.0K");
        assert_eq!(format_tokens(1_240_000), "1.2M");
    }
}
