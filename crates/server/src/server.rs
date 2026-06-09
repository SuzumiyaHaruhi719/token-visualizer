//! Embedded axum server: serves the built frontend (`dist/`) + JSON API + SSE.
//!
//! ONE server on `127.0.0.1:<port>` feeds everything: in the desktop app the
//! Tauri webviews (dashboard + tray popover are EXTERNAL webviews pointing at
//! this origin); in browser mode (`cm-serve`) a real browser tab. Because every
//! client shares this origin, no CORS is needed and the frontend uses relative
//! URLs.
//!
//! Threading: the server NEVER holds a `Store` (rusqlite `Connection` is not
//! `Sync`). It opens a short-lived read connection per request via the stored
//! `db_path`. WAL mode makes that safe alongside the watcher's writer.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
use crate::fx::{self, SharedFx};
use crate::settings;
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
    /// All currently-active sessions (drives `/api/sessions` + the session strip).
    pub sessions: Arc<RwLock<Vec<SessionState>>>,
    /// Most-recently-active session (drives `/api/current` + tray + KPI strip).
    pub current: Arc<RwLock<Option<SessionState>>>,
    /// Backfill progress `(done, total)`.
    pub import: Arc<RwLock<(usize, usize)>>,
    /// Fan-out bus for SSE.
    pub tx: broadcast::Sender<SseEvent>,
    /// Live runtime toggles shared with the tray + state-poll loop, exposed via
    /// `/api/settings`. The settings panel reads/writes these.
    pub runtime: RuntimeSettings,
    /// Billing-currency FX cache (USD-based rates), served at `/api/fx`. Seeded
    /// from `fx.json` and refreshed once/day by the `fx` thread (see fx.rs).
    pub fx: SharedFx,
}

/// The interactive runtime settings shared between the embedded server (the
/// settings panel) and the desktop subsystems (tray + chime). Each field is an
/// atomic so a `PUT /api/settings` takes effect on the next tick without any
/// restart. Volume is stored as a PERCENT (`0..=100`).
#[derive(Clone)]
pub struct RuntimeSettings {
    pub monitor_enabled: Arc<AtomicBool>,
    pub notifications_enabled: Arc<AtomicBool>,
    pub sound_enabled: Arc<AtomicBool>,
    pub sound_volume: Arc<AtomicU32>,
}

impl Default for RuntimeSettings {
    /// All toggles on, volume 80% — matches `Settings::default()`. Used by tests
    /// and any caller that does not seed from `settings.json`.
    fn default() -> Self {
        Self {
            monitor_enabled: Arc::new(AtomicBool::new(true)),
            notifications_enabled: Arc::new(AtomicBool::new(true)),
            sound_enabled: Arc::new(AtomicBool::new(true)),
            sound_volume: Arc::new(AtomicU32::new(80)),
        }
    }
}

impl AppState {
    /// Build a fresh state with an empty broadcast bus. `runtime` carries the
    /// shared on/off + volume atomics (seeded from `settings.json` in `lib.rs`).
    pub fn new(db_path: PathBuf, prices: PriceTable, runtime: RuntimeSettings) -> Self {
        let (tx, _rx) = broadcast::channel(SSE_CHANNEL_CAP);
        Self {
            db_path,
            prices: Arc::new(RwLock::new(prices)),
            sessions: Arc::new(RwLock::new(Vec::new())),
            current: Arc::new(RwLock::new(None)),
            import: Arc::new(RwLock::new((0, 0))),
            tx,
            runtime,
            // Empty until seeded from `fx.json` + refreshed by the fx thread.
            fx: Arc::new(RwLock::new(fx::FxCache::default())),
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
    // ServeDir handles `/` -> index.html and `/popover.html`, `/assets/*`, etc.
    let static_files = ServeDir::new(&dist_dir).append_index_html_on_directories(true);

    Router::new()
        .route("/api/summary", get(get_summary))
        .route("/api/current", get(get_current))
        .route("/api/sessions", get(get_sessions))
        .route("/api/pricing", get(get_pricing).put(put_pricing))
        .route("/api/limits", get(get_limits))
        .route("/api/fx", get(get_fx))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/events", get(sse_handler))
        .fallback_service(static_files)
        // Permissive CORS so the Vite dev server origin (localhost:5847) can call
        // the axum API/SSE in `tauri dev`. Same-origin in release, so it's a no-op there.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Bind `127.0.0.1:port`, spawn the server loop, and return the chosen port.
///
/// `port = 0` lets the OS pick an ephemeral port (the desktop app's behavior);
/// a fixed port (e.g. `cm-serve`'s `8788`) yields a stable/bookmarkable URL. The
/// port is returned immediately so bootstrap can build URLs / write
/// `server-port.txt`; the server itself runs forever on the async runtime. Must
/// be called from within a Tokio runtime context.
pub async fn bind(state: AppState, dist_dir: PathBuf, port: u16) -> Result<u16> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let port = listener.local_addr()?.port();
    let router = build_router(state, dist_dir);
    tokio::spawn(async move {
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
// /api/fx — USD-based billing-currency exchange rates (refreshed once/day)
// ---------------------------------------------------------------------------

/// `GET /api/fx`: the cached USD-based FX rates. Reads the in-memory cache (no
/// disk, no network) so it answers instantly; the `fx` thread keeps it fresh.
/// Returns `{ base, rates, fetchedAt, stale }`. `stale` is true when the rates
/// are older than the daily refresh window (e.g. the machine has been offline).
async fn get_fx(State(state): State<AppState>) -> Json<fx::FxResponse> {
    let cache = state.fx.read().await.clone();
    Json(fx::response_from(&cache, chrono_now_ms() / 1000))
}

// ---------------------------------------------------------------------------
// /api/settings — runtime toggles + Discord config (drives the settings panel)
// ---------------------------------------------------------------------------

/// Full settings response. Live toggles come from the runtime atomics; the
/// Discord fields are read from `settings.json` (they only take effect at
/// startup, so there is no runtime atomic for them).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsResponse {
    monitor_enabled: bool,
    notifications_enabled: bool,
    sound_enabled: bool,
    /// Volume as a `0.0..=1.0` float (stored internally as percent).
    sound_volume: f64,
    /// Popover background opacity percent (`0..=100`) for the CSS acrylic tint.
    popover_opacity: u8,
    /// Billing display currency ISO code (USD/CNY/HKD/EUR/JPY/GBP).
    currency: String,
    discord_enabled: bool,
    discord_client_id: Option<String>,
}

/// Partial settings update. Every field is optional; only the provided ones are
/// applied (to both the runtime atomics and `settings.json`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    monitor_enabled: Option<bool>,
    notifications_enabled: Option<bool>,
    sound_enabled: Option<bool>,
    sound_volume: Option<f64>,
    popover_opacity: Option<u8>,
    currency: Option<String>,
    discord_enabled: Option<bool>,
    discord_client_id: Option<String>,
}

/// Build the response from the live atomics + persisted Discord config.
fn settings_snapshot(state: &AppState) -> SettingsResponse {
    let rt = &state.runtime;
    let persisted = settings::load();
    SettingsResponse {
        monitor_enabled: rt.monitor_enabled.load(Ordering::Relaxed),
        notifications_enabled: rt.notifications_enabled.load(Ordering::Relaxed),
        sound_enabled: rt.sound_enabled.load(Ordering::Relaxed),
        sound_volume: rt.sound_volume.load(Ordering::Relaxed) as f64 / 100.0,
        popover_opacity: persisted.popover_opacity,
        currency: persisted.currency,
        discord_enabled: persisted.discord_enabled,
        discord_client_id: persisted.discord_client_id,
    }
}

/// `GET /api/settings`: current toggles + Discord config.
async fn get_settings(State(state): State<AppState>) -> Json<SettingsResponse> {
    Json(settings_snapshot(&state))
}

/// `PUT /api/settings`: apply a PARTIAL patch to the runtime atomics AND persist
/// the merged result to `settings.json`. Returns the full updated settings.
async fn put_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatch>,
) -> Json<SettingsResponse> {
    let rt = &state.runtime;
    // Start from the persisted snapshot so untouched fields are preserved.
    let mut merged = settings::load();

    if let Some(v) = patch.monitor_enabled {
        rt.monitor_enabled.store(v, Ordering::Relaxed);
        merged.monitor_enabled = v;
    }
    if let Some(v) = patch.notifications_enabled {
        rt.notifications_enabled.store(v, Ordering::Relaxed);
        merged.notifications_enabled = v;
    }
    if let Some(v) = patch.sound_enabled {
        rt.sound_enabled.store(v, Ordering::Relaxed);
        merged.sound_enabled = v;
    }
    if let Some(v) = patch.sound_volume {
        let clamped = v.clamp(0.0, 1.0);
        rt.sound_volume
            .store((clamped * 100.0).round() as u32, Ordering::Relaxed);
        merged.sound_volume = clamped;
    }
    if let Some(v) = patch.popover_opacity {
        merged.popover_opacity = v.min(100);
    }
    if let Some(v) = patch.currency {
        // Only accept known ISO codes; ignore anything else (keeps the file clean).
        const KNOWN: &[&str] = &["USD", "CNY", "HKD", "EUR", "JPY", "GBP"];
        if KNOWN.contains(&v.as_str()) {
            merged.currency = v;
        }
    }
    if let Some(v) = patch.discord_enabled {
        merged.discord_enabled = v;
    }
    if let Some(v) = patch.discord_client_id {
        // Empty string clears the id (keeps the integration off).
        merged.discord_client_id = if v.trim().is_empty() { None } else { Some(v) };
    }

    settings::save(&merged);
    Json(settings_snapshot(&state))
}

// ---------------------------------------------------------------------------
// /api/limits — per-source session usage + rate limits
// ---------------------------------------------------------------------------

/// Top-level `/api/limits` payload.
#[derive(Debug, Clone, Serialize)]
struct LimitsResponse {
    claude: ClaudeLimits,
    codex: CodexLimits,
}

/// Claude side: rate-limit remaining/reset is NOT logged locally, so only the
/// current session is exposed; the window fields are always `null`.
#[derive(Debug, Clone, Serialize)]
struct ClaudeLimits {
    session: Option<ClaudeSession>,
    #[serde(rename = "fiveHour")]
    five_hour: Option<RateWindowDto>,
    weekly: Option<RateWindowDto>,
    note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSession {
    project: String,
    model: String,
    tokens: i64,
    state: cmcore::model::PetState,
}

/// Codex side: session usage + the two rate-limit windows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexLimits {
    session: Option<CodexSession>,
    #[serde(rename = "fiveHour")]
    five_hour: Option<RateWindowDto>,
    weekly: Option<RateWindowDto>,
    plan_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexSession {
    model: String,
    tokens: i64,
}

/// A rate-limit window for the API (`remainingPercent = 100 - usedPercent`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RateWindowDto {
    used_percent: f64,
    remaining_percent: f64,
    resets_at: i64,
}

impl From<cmcore::codex::RateWindow> for RateWindowDto {
    fn from(w: cmcore::codex::RateWindow) -> Self {
        Self {
            used_percent: w.used_percent,
            remaining_percent: 100.0 - w.used_percent,
            resets_at: w.resets_at,
        }
    }
}

/// `GET /api/limits`: Claude current session (no local rate-limit data) + the
/// latest Codex rollout's cumulative usage and rate-limit windows.
async fn get_limits(State(state): State<AppState>) -> Json<LimitsResponse> {
    // Claude: reuse the live `current` session snapshot.
    let claude_session = state.current.read().await.clone().map(|c| ClaudeSession {
        project: c.project,
        model: c.model,
        tokens: c.tokens,
        state: c.state,
    });

    // Codex: read the latest rollout live (cheap; per request).
    let codex = match cmcore::paths::codex_sessions_dir() {
        Ok(dir) => cmcore::codex::latest_snapshot(&dir),
        Err(_) => None,
    };
    let codex_limits = match codex {
        Some(snap) => {
            let (primary, secondary, plan_type) = match snap.rate_limits {
                Some(rl) => (rl.primary, rl.secondary, rl.plan_type),
                None => (None, None, None),
            };
            CodexLimits {
                session: Some(CodexSession {
                    model: snap.model,
                    tokens: snap.total.total,
                }),
                five_hour: primary.map(Into::into),
                weekly: secondary.map(Into::into),
                plan_type,
            }
        }
        None => CodexLimits {
            session: None,
            five_hour: None,
            weekly: None,
            plan_type: None,
        },
    };

    Json(LimitsResponse {
        claude: ClaudeLimits {
            session: claude_session,
            five_hour: None,
            weekly: None,
            note: "remaining not exposed locally",
        },
        codex: codex_limits,
    })
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
/// `popover.html`, `assets/`). Tries, in order:
/// 1. the `CM_DIST` env override, when `allow_env` (browser mode opts in);
/// 2. `CARGO_MANIFEST_DIR/../../dist` (dev / `cargo run` from the workspace —
///    this crate lives at `crates/server`, so the workspace `dist/` is two up);
/// 3. `current_exe()/dist` and `current_exe()/../dist` (bundled layouts);
/// 4. each caller-supplied extra candidate (e.g. the Tauri resource dir's
///    `dist`, or the resource dir itself), in order;
/// 5. `cwd/dist`.
///
/// Returns the first path that exists and contains `index.html`.
///
/// `allow_env` keeps the Tauri app's resolution byte-for-byte unaffected by a
/// stray `CM_DIST` unless it explicitly opts in; `cm-serve` passes `true`.
pub fn resolve_dist_dir(extra: &[PathBuf], allow_env: bool) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Explicit env override (browser mode / CI), opt-in.
    if allow_env {
        if let Ok(d) = std::env::var("CM_DIST") {
            if !d.trim().is_empty() {
                candidates.push(PathBuf::from(d));
            }
        }
    }

    // 2. Workspace dev layout. NOTE: this crate is `crates/server`, so the
    //    workspace root (and its `dist/`) is two directories up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("..").join("..").join("dist"));

    // 3. Next to the executable (a few plausible relative layouts).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("dist"));
            candidates.push(dir.join("..").join("dist"));
        }
    }

    // 4. Caller-supplied candidates (Tauri resource dir etc.), in order.
    candidates.extend(extra.iter().cloned());

    // 5. Current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("dist"));
    }

    candidates
        .into_iter()
        .find(|p| p.join("index.html").is_file())
        .map(|p| p.canonicalize().unwrap_or(p))
}

/// Convenience wrapper used by the Tauri shell: try the workspace/exe layouts
/// plus the Tauri resource dir, WITHOUT consulting `CM_DIST` (so the desktop
/// app's behavior is independent of that env var). Mirrors the original
/// `resolve_dist_dir(resource_dir)` signature the shell used.
pub fn resolve_dist_dir_with_resource(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let extra: Vec<PathBuf> = match resource_dir {
        Some(res) => vec![res.join("dist"), res.to_path_buf()],
        None => Vec::new(),
    };
    resolve_dist_dir(&extra, false)
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
        // Log the full error chain server-side (cause + context); return only a
        // short message to the client so we never leak internals over HTTP.
        eprintln!("[api] request failed: {:#}", self.0);
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
            source: cmcore::model::Source::Claude,
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
        let state = AppState::new(db_path.clone(), PriceTable::seeded(), RuntimeSettings::default());
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
        let state = AppState::new(db_path, PriceTable::seeded(), RuntimeSettings::default());
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

    /// `/api/limits` returns 200 + the documented per-source envelope shape.
    /// Codex fields may be null on machines without `~/.codex`; the Claude note
    /// and key set are always present.
    #[tokio::test]
    async fn limits_endpoint_returns_envelope() {
        let dir = std::env::temp_dir().join(format!("cm-limits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("lim.sqlite");
        let _ = Store::open(&db_path).unwrap();
        let state = AppState::new(db_path, PriceTable::seeded(), RuntimeSettings::default());
        let app = build_router(state, dir.clone());

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/limits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Claude envelope.
        assert!(v["claude"].is_object());
        assert_eq!(
            v["claude"]["note"].as_str(),
            Some("remaining not exposed locally")
        );
        assert!(v["claude"].get("fiveHour").is_some());
        assert!(v["claude"].get("weekly").is_some());
        assert!(v["claude"].get("session").is_some());

        // Codex envelope: keys always present (values may be null).
        assert!(v["codex"].is_object());
        assert!(v["codex"].get("session").is_some());
        assert!(v["codex"].get("fiveHour").is_some());
        assert!(v["codex"].get("weekly").is_some());
        assert!(v["codex"].get("planType").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `GET /api/settings` returns 200 + the documented camelCase shape, with
    /// the live toggle values read straight from the runtime atomics.
    #[tokio::test]
    async fn settings_endpoint_reflects_runtime_atomics() {
        let dir = std::env::temp_dir().join(format!("cm-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set.sqlite");
        let _ = Store::open(&db_path).unwrap();

        // Seed non-default runtime values to prove the handler reads them.
        let runtime = RuntimeSettings {
            monitor_enabled: Arc::new(AtomicBool::new(true)),
            notifications_enabled: Arc::new(AtomicBool::new(false)),
            sound_enabled: Arc::new(AtomicBool::new(false)),
            sound_volume: Arc::new(AtomicU32::new(40)),
        };
        let state = AppState::new(db_path, PriceTable::seeded(), runtime);
        let app = build_router(state, dir.clone());

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(v["monitorEnabled"].as_bool(), Some(true));
        assert_eq!(v["notificationsEnabled"].as_bool(), Some(false));
        assert_eq!(v["soundEnabled"].as_bool(), Some(false));
        assert_eq!(v["soundVolume"].as_f64(), Some(0.40));
        // Currency + popover-opacity are always present (read from settings.json).
        assert!(v.get("popoverOpacity").is_some());
        assert!(v.get("currency").is_some());
        // Discord fields are always present (read from settings.json).
        assert!(v.get("discordEnabled").is_some());
        assert!(v.get("discordClientId").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `GET /api/fx` returns 200 + the documented `{ base, rates, fetchedAt,
    /// stale }` shape. With a fresh (empty) cache it reports USD base + stale.
    #[tokio::test]
    async fn fx_endpoint_returns_envelope() {
        let dir = std::env::temp_dir().join(format!("cm-fx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("fx.sqlite");
        let _ = Store::open(&db_path).unwrap();
        let state = AppState::new(db_path, PriceTable::seeded(), RuntimeSettings::default());
        // Seed a couple of rates so the payload is meaningful.
        {
            let mut g = state.fx.write().await;
            g.base = "USD".to_string();
            g.rates.insert("CNY".to_string(), 7.2);
            g.rates.insert("USD".to_string(), 1.0);
            g.fetched_at = chrono_now_ms() / 1000;
        }
        let app = build_router(state, dir.clone());

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/fx")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_response().into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["base"].as_str(), Some("USD"));
        assert_eq!(v["rates"]["CNY"].as_f64(), Some(7.2));
        assert!(v.get("fetchedAt").is_some());
        assert!(v.get("stale").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
