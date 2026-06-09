//! Smoke test for browser mode: boot the same axum server `cm-serve` runs and
//! assert the two endpoints the dashboard needs on first paint:
//!   * `GET /`                    -> 200 + the served index.html
//!   * `GET /api/summary?range=all` -> 200 + JSON
//!
//! This exercises the real `cmserver` server stack (router + static file
//! serving + the summary handler) against a temp SQLite store and a temp `dist/`
//! containing a minimal index.html — no Tauri, no browser, no network.

use std::time::Duration;

use cmcore::pricing::PriceTable;
use cmcore::store::Store;
use cmserver::server::{bind, AppState, RuntimeSettings};

/// Bind the server on an ephemeral port against a temp store + temp dist, then
/// hit `/` and `/api/summary` over a real TCP loopback connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_index_and_summary() {
    // Arrange: a temp dist dir with a recognizable index.html, and a temp db.
    let root = std::env::temp_dir().join(format!("cm-serve-smoke-{}", std::process::id()));
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    let marker = "<!doctype html><title>cm-serve smoke</title><h1>CM_SMOKE_OK</h1>";
    std::fs::write(dist.join("index.html"), marker).unwrap();

    let db_path = root.join("smoke.sqlite");
    let _ = Store::open(&db_path).unwrap(); // create the schema

    let state = AppState::new(db_path, PriceTable::seeded(), RuntimeSettings::default());

    // Act: bind on port 0 (ephemeral) and spawn the serve loop.
    let port = bind(state, dist.clone(), 0).await.expect("bind server");

    // GET / -> 200 + our index.html marker.
    let (status, body) = http_get(port, "/").await;
    assert_eq!(status, 200, "GET / should be 200");
    assert!(
        body.contains("CM_SMOKE_OK"),
        "GET / should return the served index.html, got: {body}"
    );

    // GET /api/summary?range=all -> 200 + JSON that parses with the expected keys.
    let (status, body) = http_get(port, "/api/summary?range=all").await;
    assert_eq!(status, 200, "GET /api/summary should be 200");
    let v: serde_json::Value = serde_json::from_str(&body).expect("summary is JSON");
    assert!(v.get("totals").is_some(), "summary has totals: {body}");
    assert!(v.get("timeseries").is_some(), "summary has timeseries");

    // GET /api/sessions -> 200 + a JSON array. This is the poll-driven endpoint
    // the live session strip reads; in browser mode `cm-serve` must spawn the
    // headless state-poll loop (with NoopHooks) for it to populate. With no live
    // sessions in this temp env it's an empty array, but the contract (200 +
    // array) must hold so a future regression that drops the poll wiring is
    // caught here rather than only in manual QA.
    let (status, body) = http_get(port, "/api/sessions").await;
    assert_eq!(status, 200, "GET /api/sessions should be 200");
    let v: serde_json::Value = serde_json::from_str(&body).expect("sessions is JSON");
    assert!(v.is_array(), "sessions is a JSON array: {body}");

    // GET /api/current -> 200 + JSON (null when no current session).
    let (status, body) = http_get(port, "/api/current").await;
    assert_eq!(status, 200, "GET /api/current should be 200");
    let _: serde_json::Value = serde_json::from_str(&body).expect("current is JSON");

    // Cleanup.
    let _ = std::fs::remove_dir_all(&root);
}

/// Minimal HTTP/1.0 client over a loopback TCP connection: returns
/// `(status_code, body)`. Avoids pulling a heavy HTTP client into the test deps.
async fn http_get(port: u16, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut stream = {
        // Retry briefly: the spawned serve loop may need a moment to accept.
        let mut last_err = None;
        let mut s = None;
        for _ in 0..50 {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(c) => {
                    s = Some(c);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
        s.unwrap_or_else(|| panic!("connect to server: {last_err:?}"))
    };

    let req =
        format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept: */*\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();

    // Split headers / body on the blank line.
    let (head, body) = match text.split_once("\r\n\r\n") {
        Some((h, b)) => (h, b.to_string()),
        None => (text.as_str(), String::new()),
    };
    // Status code is the 2nd token of the status line ("HTTP/1.1 200 OK").
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);

    (status, body)
}
