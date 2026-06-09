//! `cm-serve` — run the full Claude Monitor dashboard in a normal web browser.
//!
//! Browser mode. No Tauri window, tray, or pet — just the embedded axum server
//! (the exact same `dist/` bundle the desktop app serves) plus a browser tab
//! opened at it. Because it shares [`cmserver::run_core`] with the desktop app,
//! every dashboard animation, chart, odometer, and live token roll behaves
//! identically; only the desktop-only surfaces (tray/popover/notifications) are
//! absent.
//!
//! Cross-platform: pure Rust + the `webbrowser` crate (macOS `open` / Windows
//! `start` / Linux `xdg-open`). Configuration via env:
//! * `CM_PORT` — TCP port to bind (default `8788`); stable + bookmarkable.
//! * `CM_DIST` — explicit path to the built frontend `dist/` (otherwise
//!   resolved relative to the executable / workspace / cwd).
//! * `CM_NO_OPEN` — set to `1`/`true` to skip auto-opening the browser (useful
//!   for the smoke test, headless servers, or remote use).

use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result};
use cmserver::{run_core, RunOpts};

/// Default port. Fixed (not ephemeral) so the URL is stable across runs.
const DEFAULT_PORT: u16 = 8788;

#[tokio::main]
async fn main() -> Result<()> {
    let port_pref = read_port_env();

    // Single-instance courtesy: if a Claude Monitor server (this tool OR the
    // desktop app) is already serving on the recorded port, don't start a second
    // full runtime (duplicate watchers/backfills writing the same SQLite). Just
    // open a browser at the existing instance and exit. Best-effort; on any doubt
    // we fall through and start our own.
    if let Some(existing) = already_running() {
        let url = local_url(existing);
        println!("Claude Monitor is already running at {url} — opening your browser.");
        open_browser(&url);
        return Ok(());
    }

    // Resolve the built frontend dir (honors CM_DIST). A clear, actionable error
    // if the bundle is missing (the most common first-run mistake).
    let dist_dir = cmserver::resolve_dist_for_serve()?;

    let handle = run_core(RunOpts {
        dist_dir,
        port: port_pref,
        enable_discord: true,
    })
    .await
    .context("failed to start the Claude Monitor core runtime")?;

    // Spawn the live session-poll loop with NO desktop side effects. This is the
    // SAME loop the desktop app runs (just `NoopHooks` instead of the tray/
    // popover/notification hooks), so `/api/sessions`, `/api/current`, and the
    // poll-driven `sessions`/`usage` SSE behave identically in the browser — the
    // live session strip updates exactly as it does in the desktop window.
    cmserver::spawn_state_poll(handle.state.clone(), cmserver::NoopHooks);

    let url = local_url(handle.port);
    println!("\n  Claude Monitor — browser mode");
    println!("  Dashboard:  {url}");
    println!("  Press Ctrl-C to stop.\n");

    open_browser(&url);

    // Block until Ctrl-C. The server + worker threads run in the background; the
    // process must stay alive to keep serving.
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for Ctrl-C")?;
    println!("\nShutting down.");
    Ok(())
}

/// Read the desired port from `CM_PORT`, falling back to [`DEFAULT_PORT`] when
/// unset or unparseable.
fn read_port_env() -> u16 {
    std::env::var("CM_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// The loopback dashboard URL for a port. Host is ALWAYS `127.0.0.1` (never
/// `localhost`) to dodge the Clash-style proxy hazard documented in the app.
fn local_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

/// Detect an already-running Claude Monitor server by reading the persisted
/// `server-port.txt` and probing that port. Returns the port if something is
/// listening AND it answers `/api/summary` with JSON-ish bytes (so we don't
/// collide with an unrelated service that happens to hold the port).
fn already_running() -> Option<u16> {
    let dir = cmcore::paths::app_data_dir().ok()?;
    let raw = std::fs::read_to_string(dir.join("server-port.txt")).ok()?;
    let port: u16 = raw.trim().parse().ok()?;
    if probe_is_claude_monitor(port) {
        Some(port)
    } else {
        None
    }
}

/// Best-effort HTTP/1.0 probe of `GET /api/summary?range=all` on the loopback
/// port. Returns true only when the response looks like our JSON API (status
/// 200 with a JSON body). A short timeout keeps startup snappy when the recorded
/// port is stale/dead.
fn probe_is_claude_monitor(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut stream) = TcpStream::connect_timeout(
        &addr.parse().expect("loopback addr is valid"),
        Duration::from_millis(300),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
    use std::io::Write;
    let req = "GET /api/summary?range=all HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    // Read just enough to see the status line + a little body.
    let mut chunk = [0u8; 1024];
    let mut total = 0;
    while total < 2048 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                total += n;
            }
            Err(_) => break,
        }
    }
    let head = buf.to_ascii_lowercase();
    buf.contains("200 OK") && (head.contains("application/json") || buf.contains('{'))
}

/// Open the system default browser at `url`. Browser-mode convenience only — a
/// failure (e.g. a headless box with no browser) is logged, NOT fatal: the
/// server keeps running and the user can open the URL manually. Skipped when
/// `CM_NO_OPEN` is set.
fn open_browser(url: &str) {
    if env_flag("CM_NO_OPEN") {
        return;
    }
    if let Err(e) = webbrowser::open(url) {
        eprintln!("[cm-serve] could not auto-open a browser ({e}); open {url} manually.");
    }
}

/// Parse a boolean-ish env var (`1`/`true`/`yes`/`on`, case-insensitive).
fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        })
        .unwrap_or(false)
}
