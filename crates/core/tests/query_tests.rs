use chrono::{Local, TimeZone, Timelike};
use core::model::{ParsedEvent, Usage};
use core::query::{summary, summary_at};
use core::store::Store;

/// Epoch millis of LOCAL midnight for the day containing `now_ms`, computed the
/// same way the query layer does. Tests use this to stay deterministic across
/// machine timezones: they pin `now_ms` and compare against this, never against
/// a hard-coded UTC constant (which would only hold in one timezone).
fn local_midnight_ms(now_ms: i64) -> i64 {
    let dt = Local.timestamp_millis_opt(now_ms).single().unwrap();
    let midnight = dt.date_naive().and_hms_opt(0, 0, 0).unwrap();
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .unwrap()
        .timestamp_millis()
}

/// Day boundaries in epoch millis (UTC).
const DAY1: i64 = 1_780_653_600_000; // 2026-06-05T10:00:00Z
const DAY2: i64 = 1_780_740_000_000; // 2026-06-06T10:00:00Z

fn mk(id: &str, ts: i64, model: &str, project: &str, u: Usage) -> ParsedEvent {
    ParsedEvent {
        request_id: id.into(),
        ts,
        session_id: format!("sess-{project}"),
        project: project.into(),
        model: model.into(),
        usage: u,
        source: core::model::Source::Claude,
    }
}

fn seeded_store() -> Store {
    let s = Store::open_in_memory().unwrap();
    // Day 1, Opus, ProjA
    s.insert_event(&mk(
        "a",
        DAY1,
        "claude-opus-4-8",
        "ProjA",
        Usage {
            input: 100,
            output: 50,
            cache_create: 20,
            cache_read: 80,
            ..Default::default()
        },
    ))
    .unwrap();
    // Day 1, Sonnet, ProjB
    s.insert_event(&mk(
        "b",
        DAY1 + 60_000,
        "claude-sonnet-4-5",
        "ProjB",
        Usage {
            input: 200,
            output: 100,
            cache_create: 0,
            cache_read: 0,
            ..Default::default()
        },
    ))
    .unwrap();
    // Day 2, Opus, ProjA
    s.insert_event(&mk(
        "c",
        DAY2,
        "claude-opus-4-8",
        "ProjA",
        Usage {
            input: 300,
            output: 150,
            cache_create: 0,
            cache_read: 120,
            ..Default::default()
        },
    ))
    .unwrap();
    s
}

#[test]
fn totals_sum_all_events() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    // input: 100+200+300=600; output:50+100+150=300; cc:20; cr:80+120=200
    assert_eq!(sum.totals.input, 600);
    assert_eq!(sum.totals.output, 300);
    assert_eq!(sum.totals.cache_create, 20);
    assert_eq!(sum.totals.cache_read, 200);
    assert_eq!(sum.totals.tokens, 600 + 300 + 20 + 200);
    assert_eq!(sum.totals.messages, 3);
    assert_eq!(sum.totals.sessions, 2); // sess-ProjA, sess-ProjB
    assert_eq!(sum.range, "all");
}

#[test]
fn cache_hit_rate_formula() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    // cache_read / (input + cache_create + cache_read) = 200 / (600+20+200) = 200/820
    let expected = 200.0 / 820.0;
    assert!((sum.totals.cache_hit_rate - expected).abs() < 1e-9);
}

#[test]
fn cost_is_some_when_all_models_known() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    assert!(sum.totals.cost_usd.is_some());
    assert!(sum.totals.cost_usd.unwrap() > 0.0);
}

#[test]
fn cost_is_none_when_no_model_is_priced() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&mk(
        "x",
        DAY1,
        "totally-unknown-model",
        "ProjA",
        Usage {
            input: 100,
            ..Default::default()
        },
    ))
    .unwrap();
    let sum = summary(&s, "all").unwrap();
    assert!(sum.totals.cost_usd.is_none());
}

#[test]
fn total_cost_sums_priced_models_and_skips_unknowns() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&mk(
        "known",
        DAY1,
        "claude-sonnet-4-6",
        "ProjA",
        Usage {
            input: 1_000_000,
            output: 0,
            ..Default::default()
        },
    ))
    .unwrap();
    s.insert_event(&mk(
        "unknown",
        DAY1,
        "totally-unknown-model",
        "ProjA",
        Usage {
            input: 1_000_000,
            output: 0,
            ..Default::default()
        },
    ))
    .unwrap();

    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.totals.cost_usd, Some(3.0));
    let unknown = sum
        .by_model
        .iter()
        .find(|m| m.model == "totally-unknown-model")
        .unwrap();
    assert!(unknown.cost_usd.is_none());
}

#[test]
fn synthetic_model_counts_as_priced_zero_cost() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&mk(
        "synthetic",
        DAY1,
        "<synthetic>",
        "ProjA",
        Usage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        },
    ))
    .unwrap();

    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.totals.cost_usd, Some(0.0));
    assert_eq!(sum.by_model[0].cost_usd, Some(0.0));
}

#[test]
fn by_model_breakdown() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.by_model.len(), 2);
    // Sorted by tokens descending: Opus has 100+50+20+80 + 300+150+120 = 820;
    // Sonnet has 200+100 = 300.
    assert_eq!(sum.by_model[0].model, "claude-opus-4-8");
    assert_eq!(sum.by_model[0].tokens, 820);
    assert!(sum.by_model[0].cost_usd.is_some());
    assert_eq!(sum.by_model[1].model, "claude-sonnet-4-5");
    assert_eq!(sum.by_model[1].tokens, 300);
}

#[test]
fn empty_model_relabeled_other_and_rows_sum_to_total() {
    let s = Store::open_in_memory().unwrap();
    s.insert_event(&mk(
        "named",
        DAY1,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 100, ..Default::default() },
    ))
    .unwrap();
    // An event with no model id (some Claude records carry usage but no model).
    s.insert_event(&mk(
        "blank",
        DAY1 + 1000,
        "",
        "ProjA",
        Usage { input: 50, ..Default::default() },
    ))
    .unwrap();

    let sum = summary(&s, "all").unwrap();
    // The blank-model bucket is shown as "other" (not dropped, not blank)...
    assert_eq!(sum.by_model.len(), 2);
    let other = sum.by_model.iter().find(|m| m.model == "other").expect("an 'other' row");
    assert_eq!(other.tokens, 50);
    // ...and the per-model rows now SUM to the grand total.
    let row_sum: i64 = sum.by_model.iter().map(|m| m.tokens).sum();
    assert_eq!(row_sum, sum.totals.tokens);
    assert_eq!(sum.totals.tokens, 150);
}

#[test]
fn by_project_breakdown() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.by_project.len(), 2);
    // ProjA = 820, ProjB = 300, sorted desc.
    assert_eq!(sum.by_project[0].project, "ProjA");
    assert_eq!(sum.by_project[0].tokens, 820);
    assert_eq!(sum.by_project[1].project, "ProjB");
    assert_eq!(sum.by_project[1].tokens, 300);
}

#[test]
fn timeseries_daily_buckets() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.timeseries.len(), 2, "two distinct days");
    // Daily buckets are emitted as ISO-8601 UTC midnight (for `new Date(...)`).
    assert_eq!(sum.timeseries[0].bucket, "2026-06-05T00:00:00Z");
    assert_eq!(sum.timeseries[1].bucket, "2026-06-06T00:00:00Z");
    // Day 1 input = 100 + 200 = 300; Day 2 input = 300.
    assert_eq!(sum.timeseries[0].input, 300);
    assert_eq!(sum.timeseries[0].cache_read, 80);
    assert_eq!(sum.timeseries[1].input, 300);
    assert_eq!(sum.timeseries[1].cache_read, 120);
}

#[test]
fn empty_store_is_zeroed_not_error() {
    let s = Store::open_in_memory().unwrap();
    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.totals.tokens, 0);
    assert_eq!(sum.totals.messages, 0);
    assert_eq!(sum.totals.sessions, 0);
    assert_eq!(sum.totals.cache_hit_rate, 0.0);
    // No models -> no priced models in scope, and no rows.
    assert!(sum.totals.cost_usd.is_none());
    assert!(sum.by_model.is_empty());
    assert!(sum.timeseries.is_empty());
}

#[test]
fn range_today_filters_old_events() {
    // With an explicit "now" well after the fixture events, today excludes them.
    let s = seeded_store();
    let now = DAY2 + 10 * 24 * 60 * 60 * 1000; // ~10 days after the latest event
    let sum = summary_at(&s, "today", now).unwrap();
    assert_eq!(sum.totals.messages, 0);
    assert_eq!(sum.range, "today");
}

#[test]
fn range_today_boundary_is_local_midnight() {
    // "today" must reset at the machine's LOCAL midnight, not UTC midnight.
    // Pin a known `now_ms`, derive the local midnight for that instant, then
    // place one event one second BEFORE it (yesterday, excluded) and one event
    // exactly AT it (today, included). The boundary is inclusive.
    let s = Store::open_in_memory().unwrap();
    // 2026-06-15T12:00:00Z — a stable mid-day instant.
    const NOW: i64 = 1_781_524_800_000;
    let midnight = local_midnight_ms(NOW);

    s.insert_event(&mk(
        "yesterday",
        midnight - 1_000,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 999, ..Default::default() },
    ))
    .unwrap();
    s.insert_event(&mk(
        "at-midnight",
        midnight,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 100, ..Default::default() },
    ))
    .unwrap();
    s.insert_event(&mk(
        "later-today",
        midnight + 3_600_000,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 50, ..Default::default() },
    ))
    .unwrap();

    let sum = summary_at(&s, "today", NOW).unwrap();
    // Only the two at-or-after-local-midnight events count.
    assert_eq!(sum.totals.messages, 2);
    assert_eq!(sum.totals.input, 150);
    assert_eq!(sum.range, "today");
}

#[test]
fn timeseries_today_buckets_align_to_local_hours() {
    // "today" hourly buckets must be labelled by the event's LOCAL hour so the
    // chart aligns with the local-day boundary. Place an event at a known local
    // wall-clock hour and assert the bucket carries that hour.
    let s = Store::open_in_memory().unwrap();
    const NOW: i64 = 1_781_524_800_000; // 2026-06-15T12:00:00Z
    let midnight = local_midnight_ms(NOW);
    // 09:30 local -> should land in the "09:00" local bucket.
    let nine_thirty_local = midnight + 9 * 3_600_000 + 30 * 60_000;
    s.insert_event(&mk(
        "nine",
        nine_thirty_local,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 42, ..Default::default() },
    ))
    .unwrap();

    let sum = summary_at(&s, "today", NOW).unwrap();
    assert_eq!(sum.timeseries.len(), 1);
    // The bucket label's hour field equals the event's LOCAL hour (09).
    let expected_local_hour = Local
        .timestamp_millis_opt(nine_thirty_local)
        .single()
        .unwrap()
        .hour();
    let bucket = &sum.timeseries[0].bucket;
    let hour_field: u32 = bucket[11..13].parse().unwrap();
    assert_eq!(hour_field, expected_local_hour);
    assert_eq!(sum.timeseries[0].input, 42);
}

#[test]
fn range_7d_includes_recent_excludes_old() {
    let s = seeded_store();
    // now = 3 days after DAY2: DAY2 is within 7d, DAY1 (1 day before DAY2) too.
    let now = DAY2 + 3 * 24 * 60 * 60 * 1000;
    let sum = summary_at(&s, "7d", now).unwrap();
    assert_eq!(sum.totals.messages, 3);
    // now = 9 days after DAY2: both fixture days fall outside the 7d window.
    let now_far = DAY2 + 9 * 24 * 60 * 60 * 1000;
    let sum_far = summary_at(&s, "7d", now_far).unwrap();
    assert_eq!(sum_far.totals.messages, 0);
}

#[test]
fn range_month_filters_to_calendar_month() {
    let s = Store::open_in_memory().unwrap();
    // In the current calendar month (2026-06-05T10:00:00Z).
    s.insert_event(&mk(
        "june",
        DAY1,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 100, ..Default::default() },
    ))
    .unwrap();
    // Previous calendar month (2026-05-20T00:00:00Z) — must be excluded.
    const MAY20: i64 = 1_779_235_200_000;
    s.insert_event(&mk(
        "may",
        MAY20,
        "claude-opus-4-8",
        "ProjA",
        Usage { input: 999, ..Default::default() },
    ))
    .unwrap();

    // now = 2026-06-15T12:00:00Z: the month window starts at 2026-06-01T00:00Z.
    const NOW_JUNE15: i64 = 1_781_524_800_000;
    let sum = summary_at(&s, "month", NOW_JUNE15).unwrap();
    assert_eq!(
        sum.totals.messages, 1,
        "only the June event falls in the current calendar month"
    );
    assert_eq!(sum.totals.input, 100);
    assert_eq!(sum.range, "month");
}

#[test]
fn unknown_range_defaults_to_all() {
    let s = seeded_store();
    let sum = summary(&s, "garbage").unwrap();
    assert_eq!(sum.totals.messages, 3);
}

#[test]
fn all_seeded_events_are_claude_source() {
    let s = seeded_store();
    let sum = summary(&s, "all").unwrap();
    // Only Claude events seeded: exactly one source row, all tokens under it.
    assert_eq!(sum.by_source.len(), 1);
    let claude = &sum.by_source[0];
    assert_eq!(claude.source, core::model::Source::Claude);
    assert_eq!(claude.tokens, sum.totals.tokens);
    // Claude numbers unchanged by the new field.
    assert_eq!(sum.totals.tokens, 600 + 300 + 20 + 200);
}

#[test]
fn by_source_splits_claude_and_codex() {
    let s = seeded_store();
    // Add one Codex event mapped via the codex helper (last-usage shape).
    let last = core::codex::CodexUsage {
        input: 1000,
        cached_input: 400,
        output: 50,
        reasoning: 10,
        total: 1060,
    };
    let e = core::codex::codex_event(&last, "codex-sess", "gpt-5.4", "CodexProj", DAY2, 4096);
    s.insert_event(&e).unwrap();

    let sum = summary(&s, "all").unwrap();
    assert_eq!(sum.by_source.len(), 2);
    let codex = sum
        .by_source
        .iter()
        .find(|b| b.source == core::model::Source::Codex)
        .expect("codex source row");
    // mapped usage: input=600, cache_read=400, output=60, cache_create=0 => 1060 tokens
    assert_eq!(codex.tokens, 1060);
    assert!(codex.cost_usd.is_some(), "gpt-5.x is priced");
    let claude = sum
        .by_source
        .iter()
        .find(|b| b.source == core::model::Source::Claude)
        .expect("claude source row");
    assert_eq!(claude.tokens, 1120); // unchanged Claude total
}
