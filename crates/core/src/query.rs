//! Dashboard aggregations: `events` rows -> [`Summary`] DTO.
//!
//! All grouping happens in SQL (`GROUP BY`) over the indexed `events` table.
//! Cost is layered on in Rust via the seeded [`PriceTable`]; total `cost_usd`
//! sums priced models and is `None` only when no in-range model has a price.

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use crate::model::{
    ModelBreakdown, ProjectBreakdown, SessionState, Summary, TimeseriesBucket, Totals,
};
use crate::pricing::PriceTable;
use crate::store::Store;

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// Inclusive lower bound (epoch millis) for a range relative to `now_ms`.
/// `None` means "no lower bound" (all history).
fn range_start(range: &str, now_ms: i64) -> Option<i64> {
    match range {
        "today" => Some(start_of_utc_day(now_ms)),
        "7d" => Some(now_ms - 7 * MS_PER_DAY),
        "30d" => Some(now_ms - 30 * MS_PER_DAY),
        _ => None, // "all" and anything unknown
    }
}

/// Midnight (UTC) of the day containing `ms`.
fn start_of_utc_day(ms: i64) -> i64 {
    let days = ms.div_euclid(MS_PER_DAY);
    days * MS_PER_DAY
}

/// Build the dashboard [`Summary`] for a range, using the current time.
pub fn summary(store: &Store, range: &str) -> Result<Summary> {
    summary_at(store, range, Utc::now().timestamp_millis())
}

/// Build the dashboard [`Summary`] for a range with an explicit `now_ms`
/// (deterministic; used by tests and callers that control the clock).
pub fn summary_at(store: &Store, range: &str, now_ms: i64) -> Result<Summary> {
    let prices = PriceTable::seeded();
    summary_with(store, range, now_ms, &prices)
}

/// Build the summary against a caller-supplied price table.
pub fn summary_with(
    store: &Store,
    range: &str,
    now_ms: i64,
    prices: &PriceTable,
) -> Result<Summary> {
    let start = range_start(range, now_ms);
    let conn = store.conn();

    // Token totals + message/session counts.
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };

    let totals_sql = format!(
        "SELECT COALESCE(SUM(input),0), COALESCE(SUM(output),0), \
                COALESCE(SUM(cache_create),0), COALESCE(SUM(cache_read),0), \
                COUNT(*), COUNT(DISTINCT session_id) \
         FROM events {where_clause}"
    );
    let (input, output, cache_create, cache_read, messages, sessions): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = if let Some(s) = start {
        conn.query_row(&totals_sql, params![s], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
    } else {
        conn.query_row(&totals_sql, [], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
    };

    let by_model = query_by_model(store, start, prices)?;
    let by_project = query_by_project(store, start)?;
    let timeseries = query_timeseries(store, start)?;

    // Total cost = sum of priced per-model costs. Unknown models remain `None`
    // in `by_model`, but do not wipe out the total if other models are priced.
    let total_cost = fold_costs(by_model.iter().map(|m| m.cost_usd));

    let denom = input + cache_create + cache_read;
    let cache_hit_rate = if denom > 0 {
        cache_read as f64 / denom as f64
    } else {
        0.0
    };

    let totals = Totals {
        tokens: input + output + cache_create + cache_read,
        input,
        output,
        cache_create,
        cache_read,
        cost_usd: total_cost,
        cache_hit_rate,
        messages,
        sessions,
    };

    Ok(Summary {
        range: range.to_string(),
        totals,
        by_model,
        by_project,
        timeseries,
    })
}

/// Combine optional per-row costs: sum priced rows, returning `None` only when
/// no row had a price.
fn fold_costs(costs: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = 0.0;
    let mut priced = 0usize;
    for c in costs.flatten() {
        total += c;
        priced += 1;
    }
    if priced == 0 {
        None
    } else {
        Some(total)
    }
}

fn query_by_model(
    store: &Store,
    start: Option<i64>,
    prices: &PriceTable,
) -> Result<Vec<ModelBreakdown>> {
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT model, \
                COALESCE(SUM(input),0), COALESCE(SUM(output),0), \
                COALESCE(SUM(cache_create),0), COALESCE(SUM(cache_read),0) \
         FROM events {where_clause} \
         GROUP BY model \
         ORDER BY SUM(input+output+cache_create+cache_read) DESC"
    );
    let conn = store.conn();
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<(String, i64, i64, i64, i64)> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    };
    let rows: Vec<(String, i64, i64, i64, i64)> = if let Some(s) = start {
        stmt.query_map(params![s], map)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?
    };

    Ok(rows
        .into_iter()
        .map(|(model, input, output, cc, cr)| {
            let usage = crate::model::Usage {
                input,
                output,
                cache_create: cc,
                cache_read: cr,
                ..Default::default()
            };
            let cost_usd = prices.cost_usd(&usage, &model);
            ModelBreakdown {
                model,
                tokens: input + output + cc + cr,
                cost_usd,
            }
        })
        .collect())
}

fn query_by_project(store: &Store, start: Option<i64>) -> Result<Vec<ProjectBreakdown>> {
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT project, COALESCE(SUM(input+output+cache_create+cache_read),0) AS toks \
         FROM events {where_clause} \
         GROUP BY project \
         ORDER BY toks DESC"
    );
    let conn = store.conn();
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<ProjectBreakdown> {
        Ok(ProjectBreakdown {
            project: r.get(0)?,
            tokens: r.get(1)?,
        })
    };
    let rows = if let Some(s) = start {
        stmt.query_map(params![s], map)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?
    };
    Ok(rows)
}

fn query_timeseries(store: &Store, start: Option<i64>) -> Result<Vec<TimeseriesBucket>> {
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };
    // Convert epoch millis -> UTC date string for daily bucketing.
    let sql = format!(
        "SELECT date(ts/1000, 'unixepoch') AS bucket, \
                COALESCE(SUM(input),0), COALESCE(SUM(output),0), \
                COALESCE(SUM(cache_create),0), COALESCE(SUM(cache_read),0) \
         FROM events {where_clause} \
         GROUP BY bucket \
         ORDER BY bucket ASC"
    );
    let conn = store.conn();
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<TimeseriesBucket> {
        Ok(TimeseriesBucket {
            bucket: r.get(0)?,
            input: r.get(1)?,
            output: r.get(2)?,
            cache_create: r.get(3)?,
            cache_read: r.get(4)?,
        })
    };
    let rows = if let Some(s) = start {
        stmt.query_map(params![s], map)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?
    };
    Ok(rows)
}

/// Most-recently-active session, if any, as a [`SessionState`]. The pet/work
/// state is left at `Idle` here; live state derivation lives in `state.rs`.
/// This is a convenience for the `/api/current` endpoint when only the store
/// (not live session files) is available.
pub fn current(store: &Store) -> Result<Option<SessionState>> {
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT session_id, project, model, \
                COALESCE(SUM(input+output+cache_create+cache_read),0) AS toks, MAX(ts) \
         FROM events \
         GROUP BY session_id \
         ORDER BY MAX(ts) DESC \
         LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(r) = rows.next()? {
        Ok(Some(SessionState {
            session_id: r.get(0)?,
            project: r.get(1)?,
            model: r.get(2)?,
            state: crate::model::PetState::Idle,
            tokens: r.get(3)?,
            updated_at: r.get(4)?,
        }))
    } else {
        Ok(None)
    }
}

/// Per-session running token totals keyed by `session_id` — used by `state.rs`
/// to fill the `tokens` field of each live [`SessionState`].
pub fn session_tokens(store: &Store, session_id: &str) -> Result<i64> {
    let conn = store.conn();
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(input+output+cache_create+cache_read),0) \
         FROM events WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )?;
    Ok(n)
}
