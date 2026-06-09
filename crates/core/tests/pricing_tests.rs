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
    assert!(t.cost_usd(&Usage::default(), "claude-opus-4-7").is_some());
    assert!(t
        .cost_usd(&Usage::default(), "claude-opus-4-8-20260101")
        .is_some());
    assert!(t.cost_usd(&Usage::default(), "claude-sonnet-4-6").is_some());
    assert!(t
        .cost_usd(&Usage::default(), "claude-haiku-4-5-20251001")
        .is_some());
}

#[test]
fn angle_bracket_pseudo_models_are_zero_cost() {
    let t = PriceTable::seeded();
    let u = Usage {
        input: 1_000_000,
        output: 1_000_000,
        cache_create: 1_000_000,
        cache_read: 1_000_000,
        ..Default::default()
    };
    assert_eq!(t.cost_usd(&u, "<synthetic>"), Some(0.0));
    assert_eq!(t.cost_usd(&u, "<rollup>"), Some(0.0));
    assert_eq!(t.rate("<synthetic>").unwrap().input, 0.0);
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

#[test]
fn deepseek_models_resolve_via_prefix() {
    let t = PriceTable::seeded();
    assert!(t.cost_usd(&Usage::default(), "deepseek-v4-pro").is_some());
    assert!(t.cost_usd(&Usage::default(), "deepseek-v4-flash").is_some());
    // A future dated/suffixed id still resolves via the bare `deepseek` prefix.
    assert!(t.cost_usd(&Usage::default(), "deepseek-v5-pro-preview").is_some());
}

#[test]
fn deepseek_cache_read_uses_explicit_rate_not_0_10x_input() {
    // DeepSeek seeds an EXPLICIT cache-read rate; it must NOT be derived as
    // 0.10 × input (the Claude/Codex default), which would massively overstate
    // the cost since DeepSeek's real cache-read rate is ~0.008× input.
    let t = PriceTable::seeded();
    let r = t.rate("deepseek-v4-pro").unwrap();
    assert_eq!(r.cache_read, Some(0.0036));
    let derived = r.input * 0.10;
    assert!(
        r.cache_read_rate() < derived,
        "explicit cache-read ({}) must be far below 0.10×input ({derived})",
        r.cache_read_rate()
    );
    // 1M pure cache-read tokens cost exactly the explicit per-million rate.
    let u = Usage {
        cache_read: 1_000_000,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "deepseek-v4-pro").unwrap();
    assert!((c - 0.0036).abs() < 1e-9, "expected 0.0036, got {c}");
}

#[test]
fn deepseek_cost_matches_real_usage_within_tolerance() {
    // A real captured ~/.reasonix/usage.jsonl turn:
    //   model=deepseek-v4-pro, cacheMiss(input)=716, cacheHit(read)=32000,
    //   completion(output)=433, costUsd=0.00080417.
    // Our mapping + seeded rates must reproduce that within ~5%.
    let t = PriceTable::seeded();
    let u = Usage {
        input: 716,
        output: 433,
        cache_create: 0,
        cache_read: 32_000,
        ..Default::default()
    };
    let c = t.cost_usd(&u, "deepseek-v4-pro").unwrap();
    let actual = 0.000_804_17_f64;
    let err = (c - actual).abs() / actual;
    assert!(err < 0.05, "computed {c} vs actual {actual} (err {err})");
}

#[test]
fn cache_read_override_falls_back_to_multiplier_when_none() {
    // A `Rate::new` (no override) keeps the historical 0.10 × input behavior.
    use core::pricing::Rate;
    let r = Rate::new(2.0, 8.0);
    assert_eq!(r.cache_read, None);
    assert!((r.cache_read_rate() - 0.2).abs() < 1e-12);
}

#[test]
fn explicit_cache_read_survives_json_roundtrip() {
    let t = PriceTable::seeded();
    let json = t.to_json().unwrap();
    let back = PriceTable::from_json(&json).unwrap();
    assert_eq!(back.rate("deepseek-v4-pro").unwrap().cache_read, Some(0.0036));
}

#[test]
fn merge_seed_defaults_adds_missing_deepseek_to_old_pricing_json() {
    // Simulate an OLD pricing.json (pre-DeepSeek): a Claude-only prefix table.
    let old_json = r#"{"rates":{},"prefixes":[["claude-opus-4",{"input":5.0,"output":25.0}]]}"#;
    let mut t = PriceTable::from_json(old_json).unwrap();
    // Before merge: DeepSeek is unknown -> no cost.
    assert!(t.cost_usd(&Usage::default(), "deepseek-v4-pro").is_none());
    t.merge_seed_defaults();
    // After merge: the seeded DeepSeek prefixes are present (cost resolves).
    assert!(t.cost_usd(&Usage::default(), "deepseek-v4-pro").is_some());
    assert_eq!(t.rate("deepseek-v4-pro").unwrap().cache_read, Some(0.0036));
}

#[test]
fn merge_seed_defaults_preserves_user_overrides() {
    // A user customized an EXACT-id rate; merging seed defaults must NOT clobber
    // it (the merge only adds MISSING prefix rules, never touches `rates`).
    let custom = r#"{"rates":{"claude-opus-4-8":{"input":99.0,"output":199.0}},"prefixes":[["claude-opus-4",{"input":5.0,"output":25.0}]]}"#;
    let mut t = PriceTable::from_json(custom).unwrap();
    t.merge_seed_defaults();
    let r = t.rate("claude-opus-4-8").unwrap();
    assert_eq!(r.input, 99.0, "user's exact-id override must be kept");
    assert_eq!(r.output, 199.0);
    // The user's existing prefix is also kept (not duplicated/overwritten).
    let exists = r#"{"rates":{},"prefixes":[["claude-opus-4",{"input":7.0,"output":8.0}]]}"#;
    let mut t2 = PriceTable::from_json(exists).unwrap();
    t2.merge_seed_defaults();
    // `claude-opus-4-5` is owned ONLY by the user's `claude-opus-4` prefix (no
    // seeded longer prefix matches it), so the override still applies there.
    let r2 = t2.rate("claude-opus-4-5").unwrap();
    assert_eq!(r2.input, 7.0, "user's prefix override must survive the merge");
    // And a newly-seeded family is still added.
    assert!(t2.cost_usd(&Usage::default(), "deepseek-v4-flash").is_some());
}
