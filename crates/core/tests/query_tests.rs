use core::model::{ParsedEvent, Usage};
use core::query::{summary, summary_at};
use core::store::Store;

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
fn cost_is_none_when_any_model_unknown() {
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
    assert_eq!(sum.timeseries[0].bucket, "2026-06-05");
    assert_eq!(sum.timeseries[1].bucket, "2026-06-06");
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
    // No models -> cost is Some(0.0) (vacuously all-known), and no rows.
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
fn unknown_range_defaults_to_all() {
    let s = seeded_store();
    let sum = summary(&s, "garbage").unwrap();
    assert_eq!(sum.totals.messages, 3);
}
