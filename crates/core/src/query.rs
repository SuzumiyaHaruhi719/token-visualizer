//! Dashboard aggregations: `events` rows -> [`Summary`] DTO.
//!
//! All grouping happens in SQL (`GROUP BY`) over the indexed `events` table.
//! Cost is layered on in Rust via the seeded [`PriceTable`]; total `cost_usd`
//! sums priced models and is `None` only when no in-range model has a price.

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use rusqlite::params;

use crate::model::{
    ModelBreakdown, ProjectBreakdown, SessionState, Source, SourceBreakdown, Summary,
    TimeseriesBucket, Totals,
};
use crate::pricing::PriceTable;
use crate::store::Store;

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// Inclusive lower bound (epoch millis) for a range relative to `now_ms`.
/// `None` means "no lower bound" (all history).
///
/// "today" uses the machine's LOCAL midnight (the server runs on the user's
/// machine), so the dashboard resets when the *user's* day rolls over, not at
/// UTC midnight. This matches `today_tokens_local` (Discord). "month" stays on
/// the UTC calendar month for now — the user only flagged "today"; revisiting
/// month would also shift the popover bill and the existing month test, so it
/// is intentionally left UTC-based and called out in the summary report.
fn range_start(range: &str, now_ms: i64) -> Option<i64> {
    match range {
        "today" => Some(start_of_local_day_for_ms(now_ms)),
        "month" => Some(start_of_utc_month(now_ms)),
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

/// Epoch millis of LOCAL midnight for the day containing `now_ms`.
///
/// Deterministic in `now_ms` (no wall-clock read) but timezone-dependent: it
/// uses the machine's local offset, so tests pin `now_ms` and compare against a
/// locally-computed midnight rather than a hard-coded UTC constant. Falls back
/// to the UTC day boundary if `now_ms` is outside chrono's representable range.
fn start_of_local_day_for_ms(now_ms: i64) -> i64 {
    match Local.timestamp_millis_opt(now_ms).single() {
        Some(dt) => start_of_local_day_ms(dt),
        None => start_of_utc_day(now_ms),
    }
}

/// Signed milliseconds the local timezone is ahead of UTC at `now_ms`
/// (e.g. +8h => +28_800_000). Used to shift event timestamps so SQLite's
/// `strftime` buckets "today" by LOCAL hour. Zero if `now_ms` is unrepresentable.
fn local_utc_offset_ms(now_ms: i64) -> i64 {
    match Local.timestamp_millis_opt(now_ms).single() {
        Some(dt) => i64::from(dt.offset().local_minus_utc()) * 1000,
        None => 0,
    }
}

/// Midnight (UTC) of the first day of the calendar month containing `ms`.
/// Powers the popover's "this month" bill. Falls back to the day boundary if the
/// timestamp is somehow out of chrono's representable range.
fn start_of_utc_month(ms: i64) -> i64 {
    let dt = match Utc.timestamp_millis_opt(ms).single() {
        Some(d) => d,
        None => return start_of_utc_day(ms),
    };
    NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc().timestamp_millis())
        .unwrap_or_else(|| start_of_utc_day(ms))
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
    let by_source = query_by_source(store, start, prices)?;
    let timeseries = query_timeseries(store, start, range, now_ms)?;

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
        by_source,
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
    // Include ALL events (no model filter) so the per-model rows sum to the
    // grand total. Events with no `message.model` (some Claude records carry
    // usage but no model id) are relabeled "other" below rather than dropped —
    // dropping them made the rows add up to LESS than the total.
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
            // Unattributed (no model id) events show as "other" so the row is
            // labeled and the breakdown still sums to the grand total.
            let label = if model.is_empty() { "other".to_string() } else { model };
            ModelBreakdown {
                model: label,
                tokens: input + output + cc + cr,
                cost_usd,
            }
        })
        .collect())
}

/// Per-source token + cost breakdown. Cost is summed from per-(source,model)
/// rows so each source's models price correctly; a source's cost is `None` only
/// when no model in it has a known price.
fn query_by_source(
    store: &Store,
    start: Option<i64>,
    prices: &PriceTable,
) -> Result<Vec<SourceBreakdown>> {
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };
    let sql = format!(
        "SELECT source, model, \
                COALESCE(SUM(input),0), COALESCE(SUM(output),0), \
                COALESCE(SUM(cache_create),0), COALESCE(SUM(cache_read),0) \
         FROM events {where_clause} \
         GROUP BY source, model"
    );
    let conn = store.conn();
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<(String, String, i64, i64, i64, i64)> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    };
    let rows: Vec<(String, String, i64, i64, i64, i64)> = if let Some(s) = start {
        stmt.query_map(params![s], map)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?
    };

    // Aggregate per-(source,model) rows up to one row per source.
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<String, (i64, f64, bool)> = BTreeMap::new();
    for (source, model, input, output, cc, cr) in rows {
        let usage = crate::model::Usage {
            input,
            output,
            cache_create: cc,
            cache_read: cr,
            ..Default::default()
        };
        let entry = acc.entry(source).or_insert((0, 0.0, false));
        entry.0 += input + output + cc + cr;
        if let Some(cost) = prices.cost_usd(&usage, &model) {
            entry.1 += cost;
            entry.2 = true;
        }
    }

    Ok(acc
        .into_iter()
        .map(|(source, (tokens, cost, priced))| SourceBreakdown {
            source: Source::from_str_or_claude(&source),
            tokens,
            cost_usd: if priced { Some(cost) } else { None },
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

fn query_timeseries(
    store: &Store,
    start: Option<i64>,
    range: &str,
    now_ms: i64,
) -> Result<Vec<TimeseriesBucket>> {
    let where_clause = if start.is_some() {
        "WHERE ts >= ?1"
    } else {
        ""
    };
    // Bucket granularity depends on the range: "today" is bucketed by HOUR so the
    // chart shows the intraday usage curve (a single daily bucket renders as one
    // ugly block); every other range stays daily.
    //
    // "today" is a LOCAL day (see `range_start`), so its hourly buckets must align
    // to LOCAL hours too — otherwise the chart's hours would be offset from the
    // panel's day boundary. We shift each event's epoch seconds by the local UTC
    // offset before `strftime` (which only knows UTC/'localtime', and 'localtime'
    // is non-deterministic for tests). The emitted label keeps the `Z` suffix and
    // the frontend renders it with `toLocaleTimeString`, but the shift means the
    // wall-clock hour shown already matches the user's local hour. The daily path
    // for every other range is unchanged (still UTC midnight, `new Date(...)`).
    let offset_secs = local_utc_offset_ms(now_ms) / 1000;
    let bucket_expr = if range == "today" {
        format!("strftime('%Y-%m-%dT%H:00:00Z', ts/1000 + ({offset_secs}), 'unixepoch')")
    } else {
        "strftime('%Y-%m-%dT00:00:00Z', ts/1000, 'unixepoch')".to_string()
    };
    let sql = format!(
        "SELECT {bucket_expr} AS bucket, \
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
            // The store has no user-message text; live derivation (state.rs /
            // codex_live.rs) fills this. This DB-only convenience path leaves it
            // empty.
            last_user_message: String::new(),
            source: crate::model::Source::Claude,
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

/// Running token total for a session id SCOPED to one [`Source`].
///
/// Reasonix session names (e.g. `code-Projects`) are short and collision-prone
/// next to Claude/Codex UUIDs, so the DeepSeek live reader uses this rather than
/// [`session_tokens`] to avoid summing in a same-named session from another agent.
pub fn session_tokens_for_source(
    store: &Store,
    session_id: &str,
    source: Source,
) -> Result<i64> {
    let conn = store.conn();
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(input+output+cache_create+cache_read),0) \
         FROM events WHERE session_id = ?1 AND source = ?2",
        params![session_id, source.as_str()],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Total tokens recorded since the start of the current LOCAL day.
///
/// Uses the machine's local timezone (not UTC) so "today" matches what the
/// user perceives. Powers the Discord Rich Presence "tokens today" figure.
pub fn today_tokens_local(store: &Store) -> Result<i64> {
    today_tokens_since(store, start_of_local_day_ms(Local::now()))
}

/// Sum of all token fields for events at or after `start_ms` (epoch millis).
/// Split out from [`today_tokens_local`] so the boundary maths can be tested
/// deterministically without depending on the wall clock.
pub fn today_tokens_since(store: &Store, start_ms: i64) -> Result<i64> {
    let conn = store.conn();
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(input+output+cache_create+cache_read),0) \
         FROM events WHERE ts >= ?1",
        params![start_ms],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Epoch millis of local midnight for the day containing `now`.
fn start_of_local_day_ms(now: DateTime<Local>) -> i64 {
    let date = now.date_naive();
    let midnight = date.and_hms_opt(0, 0, 0).expect("00:00:00 is always valid");
    // `from_local_datetime` can be ambiguous around DST transitions; take the
    // earliest valid instant, falling back to the naive UTC interpretation.
    match Local.from_local_datetime(&midnight).earliest() {
        Some(dt) => dt.timestamp_millis(),
        None => midnight.and_utc().timestamp_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParsedEvent, Source, Usage};
    use crate::store::Store;

    /// Build an event whose timestamp and token totals are caller-controlled.
    fn event_at(request_id: &str, ts: i64, tokens: i64) -> ParsedEvent {
        ParsedEvent {
            request_id: request_id.to_string(),
            ts,
            session_id: "sess".to_string(),
            project: "proj".to_string(),
            model: "claude-opus-4-8".to_string(),
            usage: Usage {
                input: tokens,
                ..Default::default()
            },
            source: Source::Claude,
        }
    }

    #[test]
    fn today_tokens_since_sums_only_at_or_after_boundary() {
        let store = Store::open_in_memory().unwrap();
        let boundary = 1_700_000_000_000;
        store.insert_event(&event_at("before", boundary - 1, 100)).unwrap();
        store.insert_event(&event_at("at", boundary, 30)).unwrap();
        store.insert_event(&event_at("after", boundary + 5_000, 7)).unwrap();

        // Boundary is inclusive: the "before" event is excluded, the others summed.
        assert_eq!(today_tokens_since(&store, boundary).unwrap(), 37);
    }

    #[test]
    fn today_tokens_since_zero_on_empty_store() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(today_tokens_since(&store, 0).unwrap(), 0);
    }

    #[test]
    fn start_of_local_day_for_ms_matches_datetime_variant() {
        // The epoch-millis entry point used by `range_start("today", ..)` must
        // agree with the `DateTime`-based helper that powers Discord's "today".
        let now = Local::now();
        let now_ms = now.timestamp_millis();
        assert_eq!(start_of_local_day_for_ms(now_ms), start_of_local_day_ms(now));
    }

    #[test]
    fn start_of_local_day_for_ms_is_local_midnight() {
        // Pin a fixed instant; the computed start is <= it and lands on local
        // midnight (hour/min/sec all zero) on the same local date.
        const NOW_MS: i64 = 1_781_524_800_000; // 2026-06-15T12:00:00Z
        let start_ms = start_of_local_day_for_ms(NOW_MS);
        assert!(start_ms <= NOW_MS);
        let start_local = Local.timestamp_millis_opt(start_ms).single().unwrap();
        use chrono::Timelike;
        assert_eq!(start_local.hour(), 0);
        assert_eq!(start_local.minute(), 0);
        assert_eq!(start_local.second(), 0);
        let now_local = Local.timestamp_millis_opt(NOW_MS).single().unwrap();
        assert_eq!(start_local.date_naive(), now_local.date_naive());
    }

    #[test]
    fn start_of_local_day_is_midnight_local() {
        // Pick an arbitrary local instant; the computed start must be <= it and
        // round-trip back to local midnight (hour/min/sec all zero).
        let now = Local::now();
        let start_ms = start_of_local_day_ms(now);
        assert!(start_ms <= now.timestamp_millis());

        let start_local = Local.timestamp_millis_opt(start_ms).single().unwrap();
        use chrono::Timelike;
        assert_eq!(start_local.hour(), 0);
        assert_eq!(start_local.minute(), 0);
        assert_eq!(start_local.second(), 0);
        assert_eq!(start_local.date_naive(), now.date_naive());
    }
}
