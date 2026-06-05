use core::model::Usage;
use core::pricing::PriceTable;

#[test]
fn unknown_model_no_cost() {
    let t = PriceTable::seeded();
    assert!(t.cost_usd(&Usage::default(), "made-up").is_none());
    assert!(t.rate("made-up").is_none());
}

#[test]
fn opus_cost_math() {
    let t = PriceTable::seeded();
    let u = Usage {
        input: 1_000_000,
        output: 0,
        cache_create: 0,
        cache_read: 0,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "claude-opus-4-8").unwrap();
    assert!((c - t.rate("claude-opus-4-8").unwrap().input).abs() < 1e-9);
}

#[test]
fn output_priced_at_output_rate() {
    let t = PriceTable::seeded();
    let u = Usage {
        output: 1_000_000,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "claude-sonnet-4-5").unwrap();
    assert!((c - t.rate("claude-sonnet-4-5").unwrap().output).abs() < 1e-9);
}

#[test]
fn cache_create_is_1_25x_input() {
    let t = PriceTable::seeded();
    let rate = t.rate("claude-opus-4-8").unwrap().input;
    let u = Usage {
        cache_create: 1_000_000,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "claude-opus-4-8").unwrap();
    assert!((c - rate * 1.25).abs() < 1e-9);
}

#[test]
fn cache_read_is_0_10x_input() {
    let t = PriceTable::seeded();
    let rate = t.rate("claude-opus-4-8").unwrap().input;
    let u = Usage {
        cache_read: 1_000_000,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "claude-opus-4-8").unwrap();
    assert!((c - rate * 0.10).abs() < 1e-9);
}

#[test]
fn combined_cost_sums_components() {
    let t = PriceTable::seeded();
    let r = t.rate("claude-opus-4-8").unwrap();
    let u = Usage {
        input: 1_000_000,
        output: 1_000_000,
        cache_create: 1_000_000,
        cache_read: 1_000_000,
        ..Default::default()
    };
    let expected = r.input + r.output + r.input * 1.25 + r.input * 0.10;
    let c = t.cost_usd(&u, "claude-opus-4-8").unwrap();
    assert!((c - expected).abs() < 1e-6, "expected {expected}, got {c}");
}

#[test]
fn matches_by_model_name_prefix() {
    // Dated/suffixed model ids should still resolve via prefix.
    let t = PriceTable::seeded();
    assert!(t
        .cost_usd(&Usage::default(), "claude-opus-4-8-20260101")
        .is_some());
    assert!(t
        .cost_usd(&Usage::default(), "claude-sonnet-4-5-20251001")
        .is_some());
    assert!(t.cost_usd(&Usage::default(), "claude-haiku-4-5").is_some());
}

#[test]
fn zero_usage_known_model_is_zero_cost() {
    let t = PriceTable::seeded();
    let c = t.cost_usd(&Usage::default(), "claude-opus-4-8").unwrap();
    assert_eq!(c, 0.0);
}

#[test]
fn explicit_rate_overrides_prefix() {
    let mut t = PriceTable::seeded();
    t.set_rate("my-custom-model", 2.0, 8.0);
    let r = t.rate("my-custom-model").unwrap();
    assert_eq!(r.input, 2.0);
    assert_eq!(r.output, 8.0);
}

#[test]
fn json_roundtrip() {
    let t = PriceTable::seeded();
    let json = t.to_json().unwrap();
    let back = PriceTable::from_json(&json).unwrap();
    assert_eq!(
        back.rate("claude-opus-4-8").unwrap().input,
        t.rate("claude-opus-4-8").unwrap().input
    );
}
