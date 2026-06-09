//! Claude Monitor — headless core runtime.
//!
//! This crate is the **GUI-free** heart of Claude Monitor, shared by two front
//! ends:
//! * the Tauri desktop app (`src-tauri`), which calls [`run_core`] and then adds
//!   its window + tray + popover + notifications on top, and
//! * the `cm-serve` binary (browser mode), which calls [`run_core`] and opens a
//!   browser at the printed URL — no GUI at all.
//!
//! [`run_core`] performs the entire non-GUI bootstrap: load settings + prices,
//! build [`AppState`], seed + spawn the FX refresher, bind the axum server,
//! spawn the Claude + Codex + Reasonix backfills and live watchers, and (opt-in)
//! the Discord Rich Presence thread. It returns a [`RunHandle`] with the bound port
//! and the shared [`AppState`]. The live session-poll loop is spawned separately
//! via [`spawn_state_poll`] so each front end can pass its own
//! [`state_poll::StatePollHooks`] (desktop side effects vs. headless no-op).
//!
//! Read-only invariant: nothing under `~/.claude` or `~/.codex` is ever opened
//! for writing. All persistence (db, pricing, port file, fx, settings, lock)
//! lives under the app data dir.

pub mod discord;
pub mod fx;
pub mod server;
pub mod session_end;
pub mod settings;
pub mod state_poll;
pub mod util;

use std::path::PathBuf;

use anyhow::{Context, Result};
use cmcore::pricing::PriceTable;
use cmcore::store::Store;
use cmcore::watcher::{self, WatchEvent};

pub use server::{AppState, RuntimeSettings, SseEvent};
pub use state_poll::{NoopHooks, NotificationPrefs, StatePollHooks};

/// Options for [`run_core`].
pub struct RunOpts {
    /// The resolved directory holding the built frontend (`index.html`, …).
    pub dist_dir: PathBuf,
    /// Preferred TCP port. `0` lets the OS choose an ephemeral port (the Tauri
    /// app's behavior); a fixed port (e.g. `cm-serve`'s `8788`) gives a stable
    /// URL. The actually-bound port is returned in [`RunHandle::port`].
    pub port: u16,
    /// Whether to spawn the opt-in Discord Rich Presence thread (gated further
    /// by `settings.json`). Both front ends enable this; it self-disables when
    /// not configured.
    pub enable_discord: bool,
}

/// The result of [`run_core`]: the bound port + the shared application state.
///
/// The caller uses `port` to build URLs (Tauri windows / the browser-open URL)
/// and `state` to layer additional behavior (e.g. the Tauri tray reads
/// `state.runtime`). The async server task and all worker threads are already
/// running by the time this returns.
pub struct RunHandle {
    pub port: u16,
    pub state: AppState,
}

/// Run the entire non-GUI core: settings + prices, [`AppState`], FX, the axum
/// server, backfills, watchers, and (opt-in) Discord. Resolves once the server
/// is bound (its serve loop + the worker threads keep running in the background).
///
/// This is `async` and must be awaited **inside a Tokio runtime**, because it
/// binds the listener with `.await` and `bind` spawns the serve loop with
/// `tokio::spawn`. The spawned task outlives this call as long as the runtime
/// does, so:
/// * `cm-serve` awaits it under `#[tokio::main]` (the runtime lives for the
///   process), and
/// * the Tauri shell drives it with `tauri::async_runtime::block_on(...)`, whose
///   runtime is a persistent global (spawned tasks survive after `block_on`
///   returns — the same contract the app relied on before this refactor).
pub async fn run_core(opts: RunOpts) -> Result<RunHandle> {
    let db_path: PathBuf = cmcore::paths::default_db_path()?;
    let prices = load_prices();

    // Persisted on/off toggles + chime config, shared with the settings panel
    // via `/api/settings` and (in the desktop app) the tray + chime. Volume is
    // stored as a PERCENT (0..=100) so it fits an AtomicU32.
    let saved = settings::load();
    let runtime = RuntimeSettings {
        monitor_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            saved.monitor_enabled,
        )),
        notifications_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            saved.notifications_enabled,
        )),
        sound_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(saved.sound_enabled)),
        sound_volume: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (saved.sound_volume.clamp(0.0, 1.0) * 100.0).round() as u32,
        )),
    };

    let state = AppState::new(db_path.clone(), prices, runtime);

    // Billing-currency FX: seed the in-memory cache from `fx.json` so `/api/fx`
    // answers instantly (even offline), then spawn the once-per-day refresher on
    // its own thread (never blocks startup).
    {
        let seeded = fx::load();
        if !seeded.is_empty() {
            if let Ok(mut g) = state.fx.try_write() {
                *g = seeded;
            }
        }
        fx::spawn_refresher(state.fx.clone());
    }

    // Bind + serve the axum server. `bind` awaits the listener to learn the
    // port, then spawns the serve loop onto the ambient runtime via `tokio::spawn`.
    let port = server::bind(state.clone(), opts.dist_dir.clone(), opts.port).await?;
    write_port_file(port);

    // Backfill (own thread, own Store) for Claude + Codex + Reasonix.
    spawn_backfill(state.clone());
    spawn_codex_backfill(state.clone());
    spawn_reasonix_backfill(state.clone());

    // Live watchers + bridges to the SSE bus for Claude + Codex + Reasonix.
    spawn_watcher(state.clone(), db_path.clone());
    spawn_codex_watcher(state.clone(), db_path.clone());
    spawn_reasonix_watcher(state.clone(), db_path.clone());

    // Discord Rich Presence (own thread, own Store) — opt-in, self-disabling.
    if opts.enable_discord {
        discord::spawn(db_path.clone());
    }

    Ok(RunHandle { port, state })
}

/// Spawn the live session-poll loop on its own OS thread with the given desktop
/// `hooks`. The Tauri shell passes hooks that capture its `AppHandle` (tray
/// tooltip, popover toggle, notifications); `cm-serve` passes
/// [`state_poll::NoopHooks`]. Either way the loop publishes `sessions`/`usage`
/// to `AppState` + SSE identically.
pub fn spawn_state_poll<H: StatePollHooks>(state: AppState, hooks: H) {
    std::thread::Builder::new()
        .name("cm-state-poll".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state_poll::run(state, hooks);
            }))
            .is_err()
            {
                eprintln!("[state-poll] thread panicked");
            }
        })
        .expect("spawn state-poll thread");
}

/// Load the editable price table from `pricing.json`, falling back to the seed.
///
/// A persisted table is merged with the current seed defaults
/// ([`PriceTable::merge_seed_defaults`]) so a `pricing.json` written before a new
/// model family was added (e.g. DeepSeek) still resolves a cost for it, while the
/// user's own overrides are preserved.
fn load_prices() -> PriceTable {
    cmcore::paths::default_pricing_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| PriceTable::from_json(&s).ok())
        .map(|mut t| {
            t.merge_seed_defaults();
            t
        })
        .unwrap_or_else(PriceTable::seeded)
}

/// Persist the chosen server port to `<app_data_dir>/server-port.txt` so the
/// smoke test (and any external tooling) can discover the port.
fn write_port_file(port: u16) {
    if let Ok(dir) = cmcore::paths::app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("server-port.txt"), port.to_string());
    }
}

// ---------------------------------------------------------------------------
// Worker-thread spawns (moved verbatim from the Tauri shell; no GUI here)
// ---------------------------------------------------------------------------

/// Spawn the one-shot backfill on its own thread with its own `Store`.
/// Progress is mirrored into `AppState.import` and broadcast as `import`.
fn spawn_backfill(state: AppState) {
    std::thread::Builder::new()
        .name("cm-backfill".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
            }))
            .is_err()
            {
                eprintln!("[backfill] thread panicked");
            }
        })
        .expect("spawn backfill thread");
}

/// Spawn the one-shot Codex backfill on its own thread + Store. Read-only over
/// `~/.codex/sessions`. Does not touch the `import` progress bar (that tracks
/// the Claude backfill); failures are logged, never fatal.
fn spawn_codex_backfill(state: AppState) {
    std::thread::Builder::new()
        .name("cm-codex-backfill".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let dir = match cmcore::paths::codex_sessions_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[codex-backfill] no sessions dir: {e}");
                        return;
                    }
                };
                if !dir.is_dir() {
                    return; // Codex not installed on this machine.
                }
                let store = match Store::open(&state.db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[codex-backfill] open store: {e}");
                        return;
                    }
                };
                if let Err(e) = cmcore::importer::backfill_codex(&dir, &store, |_, _| {}) {
                    eprintln!("[codex-backfill] error: {e:#}");
                }
            }))
            .is_err()
            {
                eprintln!("[codex-backfill] thread panicked");
            }
        })
        .expect("spawn codex-backfill thread");
}

/// Spawn the one-shot Reasonix backfill on its own thread + Store. Read-only over
/// `~/.reasonix/usage.jsonl`. Mirrors [`spawn_codex_backfill`]: it does not touch
/// the `import` progress bar (that tracks the Claude backfill); failures are
/// logged, never fatal. No-ops cleanly when Reasonix is not installed.
fn spawn_reasonix_backfill(state: AppState) {
    std::thread::Builder::new()
        .name("cm-reasonix-backfill".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let usage = match cmcore::paths::reasonix_usage_path() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[reasonix-backfill] no usage path: {e}");
                        return;
                    }
                };
                if !usage.is_file() {
                    return; // Reasonix not installed / no usage log yet.
                }
                let sessions = match cmcore::paths::reasonix_sessions_dir() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[reasonix-backfill] no sessions dir: {e}");
                        return;
                    }
                };
                let store = match Store::open(&state.db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[reasonix-backfill] open store: {e}");
                        return;
                    }
                };
                if let Err(e) =
                    cmcore::importer::backfill_reasonix(&usage, &sessions, &store, |_, _| {})
                {
                    eprintln!("[reasonix-backfill] error: {e:#}");
                }
            }))
            .is_err()
            {
                eprintln!("[reasonix-backfill] thread panicked");
            }
        })
        .expect("spawn reasonix-backfill thread");
}

/// Start the live Reasonix `notify` watcher over `~/.reasonix` (acting only on
/// its `usage.jsonl`) plus a bridge thread that refreshes `current` on new
/// events. Mirrors [`spawn_codex_watcher`].
fn spawn_reasonix_watcher(state: AppState, db_path: PathBuf) {
    let dir = match cmcore::paths::reasonix_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[reasonix-watcher] no reasonix dir: {e}");
            return;
        }
    };
    if !dir.is_dir() {
        return; // Reasonix not installed: nothing to watch.
    }

    let (tx_watch, rx_watch) = std::sync::mpsc::channel::<WatchEvent>();
    let watch_store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[reasonix-watcher] open store: {e}");
            return;
        }
    };
    let handle = match watcher::watch_reasonix(&dir, watch_store, tx_watch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[reasonix-watcher] failed to start: {e}");
            return;
        }
    };

    std::thread::Builder::new()
        .name("cm-reasonix-watch-bridge".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_watch_bridge(state, db_path, rx_watch, handle);
            }))
            .is_err()
            {
                eprintln!("[reasonix-watcher] bridge thread panicked");
            }
        })
        .expect("spawn reasonix-watch-bridge thread");
}

/// Start the live Codex `notify` watcher over `~/.codex/sessions` plus a bridge
/// thread that refreshes `current` on new events. Mirrors [`spawn_watcher`].
fn spawn_codex_watcher(state: AppState, db_path: PathBuf) {
    let dir = match cmcore::paths::codex_sessions_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[codex-watcher] no sessions dir: {e}");
            return;
        }
    };
    if !dir.is_dir() {
        return; // Codex not installed: nothing to watch.
    }

    let (tx_watch, rx_watch) = std::sync::mpsc::channel::<WatchEvent>();
    let watch_store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[codex-watcher] open store: {e}");
            return;
        }
    };
    let handle = match watcher::watch_codex(&dir, watch_store, tx_watch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[codex-watcher] failed to start: {e}");
            return;
        }
    };

    std::thread::Builder::new()
        .name("cm-codex-watch-bridge".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_watch_bridge(state, db_path, rx_watch, handle);
            }))
            .is_err()
            {
                eprintln!("[codex-watcher] bridge thread panicked");
            }
        })
        .expect("spawn codex-watch-bridge thread");
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
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_watch_bridge(state, db_path, rx_watch, handle);
            }))
            .is_err()
            {
                eprintln!("[watcher] bridge thread panicked");
            }
        })
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

/// Resolve the directory holding the built frontend, with the `CM_DIST` env
/// override enabled. Used by `cm-serve`. See [`server::resolve_dist_dir`].
pub fn resolve_dist_for_serve() -> Result<PathBuf> {
    server::resolve_dist_dir(&[], true)
        .context("could not locate the built frontend `dist/` (run `npm run build`)")
}
