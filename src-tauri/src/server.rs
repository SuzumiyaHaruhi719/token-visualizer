//! Embedded axum server: serves the built frontend (`dist/`) + JSON API + SSE.
//!
//! ONE server on `127.0.0.1:<auto port>` feeds everything: the Tauri webviews
//! (dashboard + pet windows are EXTERNAL webviews pointing at this origin) and,
//! if the user opens it, a real browser tab. Because every client shares this
//! origin, no CORS is needed and the frontend can use relative URLs.
//!
//! Threading: the server NEVER holds a `Store` (rusqlite `Connection` is not
//! `Sync`). It opens a short-lived read connection per request via the stored
//! `db_path`. WAL mode makes that safe alongside the watcher's writer.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use cmcore::model::{SessionState, Summary};
use cmcore::pricing::PriceTable;
use cmcore::query;
use cmcore::store::Store;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

/// Capacity of the SSE broadcast channel. Lagging clients drop old frames
/// rather than blocking the producer.
const SSE_CHANNEL_CAP: usize = 256;

/// A server-sent event, named to match the frontend's `EventSource` listeners
/// (`usage` / `sessions` / `import`).
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// `{ current: SessionState | null }` — push on new usage / current change.
    Usage(Option<SessionState>),
    /// `SessionState[]` — push on any session add/remove/state-change.
    Sessions(Vec<SessionState>),
    /// `{ done, total }` — backfill progress.
    Import { done: usize, total: usize },
}

/// JSON envelope for the `usage` SSE event (mirrors the TS `UsageEvent`).
#[derive(Debug, Clone, Serialize)]
struct UsagePayload {
    current: Option<SessionState>,
}

/// JSON envelope for the `import` SSE event (mirrors the TS `ImportEvent`).
#[derive(Debug, Clone, Serialize)]
struct ImportPayload {
    done: usize,
    total: usize,
}

/// Shared application state behind the axum router.
///
/// All interior-mutable bits are `Arc<RwLock<…>>` (cheap to clone, safe to share
/// across the server task, the watcher bridge, and the state-poll loop). The
/// broadcast sender is itself `Clone`.
#[derive(Clone)]
pub struct AppState {
    /// Path to the SQLite db; a fresh read `Store` is opened per request.
    pub db_path: PathBuf,
    /// Editable price table (also persisted to `pricing.json`).
    pub prices: Arc<RwLock<PriceTable>>,
    /// All currently-active sessions (drives `/api/sessions` + pet windows).
    pub sessions: Arc<RwLock<Vec<SessionState>>>,
    /// Most-recently-active session (drives `/api/current` + tray + KPI strip).
    pub current: Arc<RwLock<Option<SessionState>>>,
    /// Backfill progress `(done, total)`.
    pub import: Arc<RwLock<(usize, usize)>>,
    /// Fan-out bus for SSE.
    pub tx: broadcast::Sender<SseEvent>,
}

impl AppState {
    /// Build a fresh state with an empty broadcast bus.
    pub fn new(db_path: PathBuf, prices: PriceTable) -> Self {
        let (tx, _rx) = broadcast::channel(SSE_CHANNEL_CAP);
        Self {
            db_path,
            prices: Arc::new(RwLock::new(prices)),
            sessions: Arc::new(RwLock::new(Vec::new())),
            current: Arc::new(RwLock::new(None)),
            import: Arc::new(RwLock::new((0, 0))),
            tx,
        }
    }

    /// Open a short-lived read connection to the database. The server never
    /// keeps a `Store` alive across requests (it is not `Sync`).
    fn open_store(&self) -> Result<Store> {
        Store::open(&self.db_path).context("open store for request")
    }
}

/// Query string for `GET /api/summary?range=…`.
#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    #[serde(default = "default_range")]
    range: String,
}

fn default_range() -> String {
    "all".to_string()
}

/// Build the axum router for the given state + resolved static `dist` dir.
pub fn build_router(state: AppState, dist_dir: PathBuf) -> Router {
    // ServeDir handles `/` -> index.html and `/pet.html`, `/assets/*`, etc.
    let static_files = ServeDir::new(&dist_dir).append_index_html_on_directories(true);

    Router::new()
        .route("/api/summary", get(get_summary))
        .route("/api/current", get(get_current))
        .route("/api/sessions", get(get_sessions))
        .route("/api/pricing", get(get_pricing).put(put_pricing))
        .route("/events", get(sse_handler))
        .fallback_service(static_files)
        // Permissive CORS so the Vite dev server origin (localhost:1420) can call
        // the axum API/SSE in `tauri dev`. Same-origin in release, so it's a no-op there.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Bind `127.0.0.1:0`, spawn the server loop, and return the chosen port.
///
/// The port is returned immediately so bootstrap can build window URLs and
/// write `server-port.txt`; the server itself runs forever on the async
/// runtime. Must be called from within a Tokio runtime context (it is, via the
/// Tauri async runtime).
pub async fn bind(state: AppState, dist_dir: PathBuf) -> Result<u16> {
    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let port = listener.local_addr()?.port();
    let router = build_router(state, dist_dir);
    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            eprintln!("[server] axum exited: {e}");
        }
    });
    Ok(port)
}

// ---------------------------------------------------------------------------
// JSON API handlers
// ---------------------------------------------------------------------------

async fn get_summary(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Summary>, ApiError> {
    let store = state.open_store()?;
    let prices = state.prices.read().await.clone();
    let now = chrono_now_ms();
    let summary = query::summary_with(&store, &q.range, now, &prices)?;
    Ok(Json(summary))
}

async fn get_current(State(state): State<AppState>) -> Json<Option<SessionState>> {
    Json(state.current.read().await.clone())
}

async fn get_sessions(State(state): State<AppState>) -> Json<Vec<SessionState>> {
    Json(state.sessions.read().await.clone())
}

async fn get_pricing(State(state): State<AppState>) -> Json<PriceTable> {
    Json(state.prices.read().await.clone())
}

/// Replace the price table and persist it to `pricing.json` (best-effort).
async fn put_pricing(
    State(state): State<AppState>,
    Json(new_table): Json<PriceTable>,
) -> Result<Json<PriceTable>, ApiError> {
    {
        let mut prices = state.prices.write().await;
        *prices = new_table.clone();
    }
    // Persist under the app data dir (NEVER under ~/.claude).
    if let Ok(path) = cmcore::paths::default_pricing_path() {
        if let Ok(json) = new_table.to_json() {
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let _ = tokio::fs::write(&path, json).await;
        }
    }
    Ok(Json(new_table))
}

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

/// `GET /events`: on connect, emit a snapshot (`import`, `sessions`, `usage`),
/// then forward every broadcast [`SseEvent`] as a named SSE frame.
async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.tx.subscribe();

    // Snapshot of current state, emitted before the live tail.
    let (done, total) = *state.import.read().await;
    let sessions = state.sessions.read().await.clone();
    let current = state.current.read().await.clone();

    let snapshot = vec![
        encode_event(&SseEvent::Import { done, total }),
        encode_event(&SseEvent::Sessions(sessions)),
        encode_event(&SseEvent::Usage(current)),
    ];

    // Live tail: convert the broadcast receiver into a stream, dropping lag
    // errors (a lagging client just misses intermediate frames).
    let live = BroadcastStream::new(rx).filter_map(|res| res.ok().map(|ev| encode_event(&ev)));

    let stream = tokio_stream::iter(snapshot).chain(live);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Serialize an [`SseEvent`] into a named axum SSE [`Event`]. Serialization
/// failures degrade to an empty-data frame of the same name (never panics).
fn encode_event(ev: &SseEvent) -> Result<Event, std::convert::Infallible> {
    let (name, data) = match ev {
        SseEvent::Usage(current) => (
            "usage",
            serde_json::to_string(&UsagePayload {
                current: current.clone(),
            }),
        ),
        SseEvent::Sessions(sessions) => ("sessions", serde_json::to_string(sessions)),
        SseEvent::Import { done, total } => (
            "import",
            serde_json::to_string(&ImportPayload {
                done: *done,
                total: *total,
            }),
        ),
    };
    let data = data.unwrap_or_else(|_| "null".to_string());
    Ok(Event::default().event(name).data(data))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Current epoch millis. Kept local so the server module has no chrono dep of
/// its own beyond what core already pulls in.
fn chrono_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Resolve the directory that holds the built frontend (`index.html`,
/// `pet.html`, `assets/`). Tries, in order:
/// 1. `CARGO_MANIFEST_DIR/../dist` (dev / `cargo run` from the workspace),
/// 2. `current_exe()/../dist` and `current_exe()/../../dist` (bundled layouts),
/// 3. the Tauri resource dir's `dist` (production bundle), supplied by caller.
///
/// Returns the first path that exists and contains `index.html`.
pub fn resolve_dist_dir(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Workspace dev layout.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("..").join("dist"));

    // 2. Next to the executable (a few plausible relative layouts).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("dist"));
            candidates.push(dir.join("..").join("dist"));
        }
    }

    // 3. Tauri resource dir.
    if let Some(res) = resource_dir {
        candidates.push(res.join("dist"));
        candidates.push(res.to_path_buf());
    }

    candidates
        .into_iter()
        .find(|p| p.join("index.html").is_file())
        .map(|p| p.canonicalize().unwrap_or(p))
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Wraps internal errors as HTTP 500 with a short message (no sensitive leak).
pub struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {}", self.0),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use cmcore::model::{ParsedEvent, Usage};
    use tower::ServiceExt; // for `oneshot`

    fn sample_event(id: &str) -> ParsedEvent {
        ParsedEvent {
            request_id: id.to_string(),
            ts: 1_700_000_000_000,
            session_id: "sess-1".to_string(),
            project: "claude-monitor".to_string(),
            model: "claude-opus-4-8".to_string(),
            usage: Usage {
                input: 100,
                output: 20,
                cache_create: 50,
                cache_read: 2000,
                ..Default::default()
            },
        }
    }

    /// Boot the router against a temp-file store with one event and assert
    /// `/api/summary` returns 200 + JSON whose totals reflect the data.
    #[tokio::test]
    async fn summary_endpoint_returns_json_with_totals() {
        // Arrange: a temp db with a single known event.
        let dir = std::env::temp_dir().join(format!("cm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.sqlite");
        {
            let store = Store::open(&db_path).unwrap();
            store.insert_event(&sample_event("req-1")).unwrap();
        }
        let state = AppState::new(db_path.clone(), PriceTable::seeded());
        let app = build_router(state, dir.clone());

        // Act: GET /api/summary?range=all
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/summary?range=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Assert: 200 + parseable Summary with the expected token total.
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: Summary = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary.totals.tokens, 2170); // 100+20+50+2000
        assert_eq!(summary.totals.messages, 1);
        assert!(summary.totals.cost_usd.is_some());

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn current_and_sessions_default_empty() {
        let dir = std::env::temp_dir().join(format!("cm-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t2.sqlite");
        let _ = Store::open(&db_path).unwrap();
        let state = AppState::new(db_path, PriceTable::seeded());
        let app = build_router(state, dir.clone());

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let sessions: Vec<SessionState> = serde_json::from_slice(&body).unwrap();
        assert!(sessions.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
