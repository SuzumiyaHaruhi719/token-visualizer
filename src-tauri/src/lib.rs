//! Claude Monitor — Tauri desktop shell.
//!
//! Wires the pure-logic `cmcore` crate into a running desktop app:
//! * an embedded axum server (`server`) on `127.0.0.1:<auto port>` that serves
//!   the built frontend + JSON API + SSE,
//! * a backfill pass + live `notify` watcher that keep the SQLite store current,
//! * a state-poll loop (`state_poll`) that derives live per-session pet state,
//! * the desktop surface: a dashboard window, per-session pet windows
//!   (`windows`), and a system tray (`tray`).
//!
//! Read-only invariant: nothing under `~/.claude` is ever opened for writing.
//! All persistence (db, pricing, port file) lives under the app data dir.

mod server;
mod state_poll;
mod tray;
mod windows;

use std::path::PathBuf;

use cmcore::pricing::PriceTable;
use cmcore::store::Store;
use cmcore::watcher::{self, WatchEvent};
use server::{AppState, SseEvent};
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

/// Load the editable price table from `pricing.json`, falling back to the seed.
fn load_prices() -> PriceTable {
    cmcore::paths::default_pricing_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| PriceTable::from_json(&s).ok())
        .unwrap_or_else(PriceTable::seeded)
}

/// Persist the chosen server port to `<app_data_dir>/server-port.txt` so the
/// smoke test (and any external tooling) can discover the ephemeral port.
fn write_port_file(port: u16) {
    if let Ok(dir) = cmcore::paths::app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("server-port.txt"), port.to_string());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure_proxy_bypass();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // --- shared state -------------------------------------------------
            let db_path: PathBuf = cmcore::paths::default_db_path()?;
            let prices = load_prices();
            let state = AppState::new(db_path.clone(), prices);

            // --- resolve the built frontend dir ------------------------------
            let resource_dir = app.path().resource_dir().ok();
            let dist_dir = server::resolve_dist_dir(resource_dir.as_deref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "could not locate the built frontend `dist/` (run `npm run build`)"
                )
            })?;

            // --- bind + serve the axum server --------------------------------
            // Bind synchronously to learn the port; the serve loop is spawned
            // onto the async runtime inside `bind`.
            let port = tauri::async_runtime::block_on(server::bind(
                state.clone(),
                dist_dir.clone(),
            ))?;
            write_port_file(port);

            // --- backfill (own thread, own Store) ----------------------------
            spawn_backfill(state.clone());

            // --- live watcher + bridge to the SSE bus ------------------------
            spawn_watcher(state.clone(), db_path.clone());

            // --- state-poll loop (live pet state + windows + tray) -----------
            {
                let state = state.clone();
                let app_for_poll = handle.clone();
                std::thread::Builder::new()
                    .name("cm-state-poll".into())
                    .spawn(move || state_poll::run(state, app_for_poll, port))
                    .expect("spawn state-poll thread");
            }

            // --- desktop surface ---------------------------------------------
            windows::create_dashboard(&handle, port)?;
            tray::build(&handle, port)?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Spawn the one-shot backfill on its own thread with its own `Store`.
/// Progress is mirrored into `AppState.import` and broadcast as `import`.
fn spawn_backfill(state: AppState) {
    std::thread::Builder::new()
        .name("cm-backfill".into())
        .spawn(move || {
            let projects = match cmcore::paths::projects_dir() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[backfill] no projects dir: {e}");
                    return;
                }
            };
            let store = match Store::open(&state.db_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[backfill] open store: {e}");
                    return;
                }
            };
            let tx = state.tx.clone();
            let import = state.import.clone();
            let res = cmcore::importer::backfill(&projects, &store, |done, total| {
                {
                    let mut g = import.blocking_write();
                    *g = (done, total);
                }
                let _ = tx.send(SseEvent::Import { done, total });
            });
            if let Err(e) = res {
                eprintln!("[backfill] error: {e:#}");
            }
        })
        .expect("spawn backfill thread");
}

/// Start the live `notify` watcher (which owns its own moved `Store`) and a
/// bridge thread that turns each [`WatchEvent`] into a `usage` broadcast.
fn spawn_watcher(state: AppState, db_path: PathBuf) {
    let projects = match cmcore::paths::projects_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[watcher] no projects dir: {e}");
            return;
        }
    };

    // Channel from the watcher thread to our bridge.
    let (tx_watch, rx_watch) = std::sync::mpsc::channel::<WatchEvent>();

    // The watcher needs its own Store (moved in).
    let watch_store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[watcher] open store: {e}");
            return;
        }
    };

    let handle = match watcher::watch(&projects, watch_store, tx_watch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[watcher] failed to start: {e}");
            return;
        }
    };

    // Bridge thread: keep the WatchHandle alive + recompute `current` on events.
    std::thread::Builder::new()
        .name("cm-watch-bridge".into())
        .spawn(move || run_watch_bridge(state, db_path, rx_watch, handle))
        .expect("spawn watch-bridge thread");
}

/// Drain watcher events and broadcast a refreshed `usage` snapshot. Holds the
/// [`watcher::WatchHandle`] for the app's lifetime so the watcher stays alive.
fn run_watch_bridge(
    state: AppState,
    db_path: PathBuf,
    rx: std::sync::mpsc::Receiver<WatchEvent>,
    _handle: watcher::WatchHandle,
) {
    for _ev in rx.iter() {
        // The watcher already inserted the events into the store; we just need
        // to refresh the dashboard's notion of "current" from the store.
        if let Ok(store) = Store::open(&db_path) {
            if let Ok(Some(c)) = cmcore::query::current(&store) {
                let _ = state.tx.send(SseEvent::Usage(Some(c)));
            }
        }
    }
}
