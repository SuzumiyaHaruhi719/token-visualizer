//! Claude Monitor — Tauri desktop shell.
//!
//! Thin GUI layer over the shared [`cmserver`] core runtime. The heavy lifting
//! (embedded axum server, SQLite ingestion + watchers, FX, settings, the live
//! session-poll publish path, Discord) lives in `cmserver::run_core`; this crate
//! adds only the desktop surface:
//! * a dashboard window + tray popover (`windows`) and a system tray (`tray`),
//! * session-end notifications (`notify`),
//! * the desktop side-effects of the state-poll loop, supplied as
//!   [`state_poll::DesktopHooks`].
//!
//! The exact same `cmserver::run_core` powers the headless `cm-serve` binary
//! (browser mode), so the dashboard behaves identically in a browser tab — only
//! the tray/popover/notifications (this crate) are desktop-only.
//!
//! Read-only invariant: nothing under `~/.claude` is ever opened for writing.
//! All persistence (db, pricing, port file) lives under the app data dir.

mod notify;
mod state_poll;
mod tray;
mod windows;

use cmserver::{run_core, RunOpts};
use tauri::Manager;

/// Configure the embedded WebView2 + this process to bypass any system proxy for
/// localhost. A Clash-style HTTP(S)_PROXY with an empty NO_PROXY otherwise hangs
/// `127.0.0.1` requests (see the `clash-proxy-breaks-localhost` note).
fn configure_proxy_bypass() {
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--proxy-bypass-list=127.0.0.1;localhost;*.localhost",
    );
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure_proxy_bypass();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        // Frontend-driven auto-fit: the popover measures its content and asks the
        // shell to snap the window to that exact height (no dead whitespace).
        .invoke_handler(tauri::generate_handler![windows::set_popover_height])
        .setup(|app| {
            let handle = app.handle().clone();

            // macOS: run as a menu-bar (Accessory) app, mirroring the tray-first
            // design on Windows. The dashboard + popover are created hidden and
            // revealed from the tray, so a Dock icon (and the default "Regular"
            // policy that expects a visible window) would be wrong here — it would
            // bounce in the Dock with no window to show. Accessory keeps the app
            // alive in the menu bar with no Dock presence. macOS-only: the method
            // does not exist on other platforms.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // --- resolve the built frontend dir ------------------------------
            // Desktop resolution is independent of the `CM_DIST` env override
            // (that is a browser-mode/`cm-serve` affordance), so the app behaves
            // exactly as before. The Tauri resource dir is offered as a
            // candidate for the bundled layout.
            let resource_dir = app.path().resource_dir().ok();
            let dist_dir = cmserver::server::resolve_dist_dir_with_resource(resource_dir.as_deref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "could not locate the built frontend `dist/` (run `npm run build`)"
                    )
                })?;

            // --- shared core runtime (server + ingestion + fx + discord) -----
            // Port 0 = OS-chosen ephemeral port, exactly as the app used before.
            // `run_core` binds the server, writes `server-port.txt`, and spawns
            // the backfills + watchers + fx + (opt-in) Discord.
            //
            // `run_core` is async (it `.await`s the bind and `tokio::spawn`s the
            // serve loop), so it must be driven inside a Tokio runtime. The Tauri
            // `setup` closure is NOT itself in one, so we drive it on Tauri's
            // PERSISTENT global async runtime via `block_on`; the spawned axum
            // task then lives on that runtime for the app's lifetime (the same
            // contract the app relied on before this refactor).
            let run = tauri::async_runtime::block_on(run_core(RunOpts {
                dist_dir,
                port: 0,
                enable_discord: true,
            }))?;
            let port = run.port;
            let state = run.state;

            // --- state-poll loop with DESKTOP side effects -------------------
            // The publish path (sessions/usage -> AppState + SSE) lives in
            // `cmserver`; here we supply the tray tooltip + popover toggle +
            // notification hooks, which capture the AppHandle.
            let monitor_enabled = state.runtime.monitor_enabled.clone();
            cmserver::spawn_state_poll(
                state.clone(),
                state_poll::DesktopHooks::new(handle.clone(), port),
            );

            // --- desktop surface ---------------------------------------------
            windows::create_dashboard(&handle, port)?;
            // Tray "today" popover: created hidden. Reveal it now when the monitor
            // is enabled so turning the monitor on actually shows it (the state-
            // poll loop also shows/hides it as the toggle flips); otherwise it
            // waits for a tray click.
            windows::create_popover(&handle, port)?;
            if monitor_enabled.load(std::sync::atomic::Ordering::Relaxed) {
                windows::show_popover(&handle, port);
            }
            tray::build(&handle, port, monitor_enabled.clone())?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
