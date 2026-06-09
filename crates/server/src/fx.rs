//! Billing-currency FX rates.
//!
//! ALL cost figures in this app are computed in USD (the Claude/Codex price
//! tables are USD). To let the UI show spend in the user's currency we fetch
//! USD-based exchange rates ONCE PER DAY from a free, no-key public API and let
//! the frontend convert on display (see `src/lib/currency.ts`).
//!
//! Design (audited with Codex):
//! * Rates live in an `Arc<RwLock<FxCache>>` held by `AppState`, seeded from
//!   `<app_data_dir>/fx.json` at startup so `/api/fx` answers instantly (even
//!   offline) and we never read the disk per request.
//! * A dedicated `std::thread` (NOT the tokio runtime) does the blocking HTTP.
//!   `reqwest::blocking` must never run inside an async handler. Startup is
//!   never blocked — the thread is spawned and the server serves cached/empty
//!   data until rates land.
//! * Refetch only when missing or older than [`REFRESH_SECS`] (24h). On failure
//!   we keep serving the stale cache and back off (exponential + jitter, capped)
//!   via `next_attempt_at` so two dead APIs can't be hammered.
//! * Primary API: `open.er-api.com` (broad coverage incl. CNY/HKD/JPY/EUR/GBP).
//!   Fallback: `frankfurter.app` (follows its 301 to frankfurter.dev).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Refetch rates only when the cache is older than this (24h). FX moves slowly;
/// once a day is plenty for cost display and is gentle on the free APIs.
const REFRESH_SECS: i64 = 24 * 60 * 60;
/// HTTP timeout per attempt. Kept short so a hung endpoint falls through to the
/// fallback (and then to the stale cache) quickly.
const HTTP_TIMEOUT_SECS: u64 = 12;
/// Backoff after a failed refresh: first retry, then doubling up to the cap.
const BACKOFF_BASE_SECS: i64 = 60;
const BACKOFF_MAX_SECS: i64 = 6 * 60 * 60;
/// Currencies we care about (kept from the primary's broad table). Storing only
/// these keeps `fx.json` tiny and the `/api/fx` payload focused.
const KEPT: &[&str] = &["CNY", "HKD", "EUR", "JPY", "GBP", "USD"];

/// The on-disk + in-memory cache. `rates[X]` = units of X per 1 USD.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FxCache {
    /// Always "USD" — rates are USD-based.
    pub base: String,
    /// USD-based rates (units per 1 USD). USD itself is implicitly 1.0.
    pub rates: BTreeMap<String, f64>,
    /// Epoch seconds the rates were last successfully fetched (0 = never).
    pub fetched_at: i64,
    /// Epoch seconds before which we must NOT retry (failure backoff). Not
    /// serialized's concern for the API; skipped from the wire payload.
    #[serde(skip)]
    pub next_attempt_at: i64,
}

impl FxCache {
    /// True when there are no usable rates yet (never fetched / empty file).
    pub fn is_empty(&self) -> bool {
        self.rates.is_empty()
    }
    /// True when the cache is older than the refresh window (or never fetched).
    fn is_stale(&self, now: i64) -> bool {
        self.fetched_at <= 0 || now.saturating_sub(self.fetched_at) >= REFRESH_SECS
    }
}

/// The `/api/fx` JSON payload (camelCase). `stale` is true when we are serving
/// rates older than the refresh window (e.g. offline) so the UI could note it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FxResponse {
    pub base: String,
    pub rates: BTreeMap<String, f64>,
    pub fetched_at: i64,
    pub stale: bool,
}

/// Shared FX handle stored in `AppState`. Cheap to clone (Arc).
pub type SharedFx = Arc<RwLock<FxCache>>;

/// `<app_data_dir>/fx.json` — the persisted cache (NEVER under ~/.claude).
fn fx_path() -> Option<PathBuf> {
    cmcore::paths::app_data_dir().ok().map(|d| d.join("fx.json"))
}

/// Load the persisted cache from disk (empty default if missing/corrupt).
pub fn load() -> FxCache {
    fx_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<FxCache>(&s).ok())
        .unwrap_or_default()
}

/// Persist the cache atomically (temp file + rename) so a crash mid-write can't
/// leave a half-written `fx.json`. Best-effort; failures are ignored.
fn save(cache: &FxCache) {
    let Some(path) = fx_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Build the wire payload from the cache at time `now`.
pub fn response_from(cache: &FxCache, now: i64) -> FxResponse {
    FxResponse {
        base: if cache.base.is_empty() {
            "USD".to_string()
        } else {
            cache.base.clone()
        },
        rates: cache.rates.clone(),
        fetched_at: cache.fetched_at,
        stale: cache.is_stale(now),
    }
}

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Tiny deterministic-ish jitter in `0..range` seconds, derived from the clock
/// (no rng dep). Spreads retries so repeated failures don't align.
fn jitter(range: i64) -> i64 {
    if range <= 0 {
        return 0;
    }
    (now_secs().unsigned_abs() % range as u64) as i64
}

/// Keep only the currencies we display, and guarantee USD = 1.0 is present.
fn keep_relevant(mut rates: BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    rates.retain(|k, v| KEPT.contains(&k.as_str()) && v.is_finite() && *v > 0.0);
    rates.insert("USD".to_string(), 1.0);
    rates
}

// --- API response shapes ---------------------------------------------------

/// `open.er-api.com/v6/latest/USD` → `{ result, base_code, rates, time_last_update_unix }`.
#[derive(Debug, Deserialize)]
struct ErApiResponse {
    result: String,
    #[serde(default)]
    rates: BTreeMap<String, f64>,
    #[serde(default)]
    time_last_update_unix: i64,
}

/// `frankfurter.app/latest?from=USD` → `{ base, date, rates }` (USD-based).
#[derive(Debug, Deserialize)]
struct FrankfurterResponse {
    #[serde(default)]
    rates: BTreeMap<String, f64>,
}

/// Fetch from the primary API. Returns `(rates, fetched_at)` on success.
fn fetch_primary(client: &reqwest::blocking::Client) -> anyhow::Result<(BTreeMap<String, f64>, i64)> {
    let resp: ErApiResponse = client
        .get("https://open.er-api.com/v6/latest/USD")
        .send()?
        .error_for_status()?
        .json()?;
    if resp.result != "success" || resp.rates.is_empty() {
        anyhow::bail!("primary FX result not success / empty");
    }
    let fetched = if resp.time_last_update_unix > 0 {
        resp.time_last_update_unix
    } else {
        now_secs()
    };
    Ok((keep_relevant(resp.rates), fetched))
}

/// Fetch from the fallback API (reqwest follows the 301 to frankfurter.dev).
fn fetch_fallback(client: &reqwest::blocking::Client) -> anyhow::Result<(BTreeMap<String, f64>, i64)> {
    let resp: FrankfurterResponse = client
        .get("https://api.frankfurter.app/latest?from=USD")
        .send()?
        .error_for_status()?
        .json()?;
    if resp.rates.is_empty() {
        anyhow::bail!("fallback FX empty");
    }
    Ok((keep_relevant(resp.rates), now_secs()))
}

/// One refresh attempt: primary then fallback. On success, update the shared
/// cache + persist; on failure, set a backoff `next_attempt_at` and keep stale.
fn refresh_once(shared: &SharedFx, client: &reqwest::blocking::Client, attempt: u32) {
    let result = fetch_primary(client).or_else(|e1| {
        eprintln!("[fx] primary failed: {e1:#}; trying fallback");
        fetch_fallback(client)
    });

    let now = now_secs();
    match result {
        Ok((rates, fetched_at)) => {
            let updated = FxCache {
                base: "USD".to_string(),
                rates,
                fetched_at,
                next_attempt_at: 0,
            };
            // Persist outside the lock-free section; clone is cheap.
            save(&updated);
            let mut guard = shared.blocking_write();
            *guard = updated;
        }
        Err(e) => {
            // Exponential backoff with jitter, capped. Keeps two dead APIs from
            // being hammered while still recovering on its own.
            let backoff = (BACKOFF_BASE_SECS.saturating_mul(1i64 << attempt.min(8)))
                .min(BACKOFF_MAX_SECS)
                + jitter(BACKOFF_BASE_SECS);
            eprintln!("[fx] refresh failed: {e:#}; backing off {backoff}s");
            let mut guard = shared.blocking_write();
            guard.next_attempt_at = now + backoff;
        }
    }
}

/// Spawn the once-per-day FX refresher on its own thread. Never blocks startup.
///
/// Loop: if the cache is fresh, sleep until it would expire; if stale and the
/// backoff window has passed, attempt a refresh. The thread lives for the app's
/// lifetime. All HTTP is blocking and confined to THIS thread.
pub fn spawn_refresher(shared: SharedFx) {
    std::thread::Builder::new()
        .name("cm-fx".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let client = match reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
                    .user_agent("claude-monitor-fx/1.0")
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[fx] could not build http client: {e:#}");
                        return;
                    }
                };

                let mut attempt: u32 = 0;
                loop {
                    let now = now_secs();
                    let (stale, next_attempt) = {
                        let g = shared.blocking_read();
                        (g.is_stale(now), g.next_attempt_at)
                    };

                    if stale && now >= next_attempt {
                        refresh_once(&shared, &client, attempt);
                        // Track consecutive failures for backoff growth.
                        let failed = {
                            let g = shared.blocking_read();
                            g.is_stale(now_secs())
                        };
                        attempt = if failed { attempt.saturating_add(1) } else { 0 };
                    }

                    // Sleep until the cache would next need attention: either the
                    // refresh window from the last fetch, or the backoff window.
                    let sleep_secs = {
                        let g = shared.blocking_read();
                        let n = now_secs();
                        if g.is_stale(n) {
                            // Waiting on a backoff window (or first-ever fetch).
                            (g.next_attempt_at - n).max(30)
                        } else {
                            // Fresh: wake a bit after it expires.
                            (g.fetched_at + REFRESH_SECS - n).max(60)
                        }
                    };
                    std::thread::sleep(Duration::from_secs(sleep_secs.min(REFRESH_SECS) as u64));
                }
            }))
            .is_err()
            {
                eprintln!("[fx] refresher thread panicked");
            }
        })
        .expect("spawn fx refresher thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_relevant_filters_and_forces_usd() {
        let mut raw = BTreeMap::new();
        raw.insert("CNY".to_string(), 7.2);
        raw.insert("XYZ".to_string(), 3.3); // dropped (not displayed)
        raw.insert("EUR".to_string(), 0.0); // dropped (non-positive)
        raw.insert("GBP".to_string(), 0.79);
        let kept = keep_relevant(raw);
        assert_eq!(kept.get("CNY"), Some(&7.2));
        assert_eq!(kept.get("GBP"), Some(&0.79));
        assert_eq!(kept.get("USD"), Some(&1.0)); // always present
        assert!(!kept.contains_key("XYZ"));
        assert!(!kept.contains_key("EUR")); // zero rate dropped
    }

    #[test]
    fn staleness_respects_refresh_window() {
        let now = 1_000_000;
        let fresh = FxCache {
            base: "USD".into(),
            rates: BTreeMap::new(),
            fetched_at: now - 100,
            next_attempt_at: 0,
        };
        assert!(!fresh.is_stale(now));
        let old = FxCache {
            fetched_at: now - REFRESH_SECS - 1,
            ..fresh.clone()
        };
        assert!(old.is_stale(now));
        let never = FxCache::default();
        assert!(never.is_stale(now));
    }

    #[test]
    fn response_defaults_base_to_usd_and_flags_stale() {
        let now = 2_000_000;
        let empty = FxCache::default();
        let r = response_from(&empty, now);
        assert_eq!(r.base, "USD");
        assert!(r.stale);
        assert!(r.rates.is_empty());
    }
}
