//! Editable price table and cost estimation.
//!
//! Cost is an *estimate* against Anthropic's public per-token API pricing.
//! Rates are plain data: the seeded table is overridable from a `pricing.json`
//! file, so getting an exact number right is a config concern, not a code one.
//!
//! Billing model (per the design, §5.4):
//! * `input`  tokens billed at the model's input rate
//! * `output` tokens billed at the model's output rate
//! * `cache_create` (cache write) billed at 1.25x the input rate
//! * `cache_read`  (cache hit)   billed at 0.10x the input rate
//!
//! All rates are USD per **million** tokens.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::Usage;

/// Multiplier applied to the input rate for cache-write (cache_creation) tokens.
pub const CACHE_CREATE_MULTIPLIER: f64 = 1.25;
/// Multiplier applied to the input rate for cache-read (cache hit) tokens.
pub const CACHE_READ_MULTIPLIER: f64 = 0.10;
/// One million — rates are expressed per million tokens.
const PER_MILLION: f64 = 1_000_000.0;

/// Input/output USD rates for a model, per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    /// USD per million input tokens.
    pub input: f64,
    /// USD per million output tokens.
    pub output: f64,
    /// Optional ABSOLUTE USD-per-million rate for cache-read (cache-hit) tokens.
    ///
    /// `None` (the default for Claude / Codex) keeps the historical billing rule
    /// of [`CACHE_READ_MULTIPLIER`] × `input`. `Some(rate)` overrides that with an
    /// explicit per-million rate — needed because some providers (e.g. DeepSeek
    /// via Reasonix) price cache hits at a multiplier nowhere near 0.10× input,
    /// so deriving them from `input` would skew the total cost badly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
}

impl Rate {
    /// A rate with no explicit cache-read override (cache reads fall back to
    /// [`CACHE_READ_MULTIPLIER`] × `input`).
    pub const fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: None,
        }
    }

    /// A rate with an explicit absolute cache-read USD-per-million rate.
    pub const fn with_cache_read(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input,
            output,
            cache_read: Some(cache_read),
        }
    }

    /// The effective USD-per-million cache-read rate: the explicit override when
    /// set, else [`CACHE_READ_MULTIPLIER`] × `input` (the Claude/Codex default).
    pub fn cache_read_rate(&self) -> f64 {
        self.cache_read
            .unwrap_or(self.input * CACHE_READ_MULTIPLIER)
    }
}

/// A table of model rates, resolved by pseudo-model, exact id, then name prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    /// Exact-id overrides for real model ids.
    pub rates: HashMap<String, Rate>,
    /// Prefix rules: if a model id starts with `prefix`, use this `Rate`.
    /// Checked longest-prefix-first so more specific prefixes win.
    pub prefixes: Vec<(String, Rate)>,
}

impl Default for PriceTable {
    fn default() -> Self {
        Self::seeded()
    }
}

impl PriceTable {
    /// Seed with Anthropic's public Claude 4.x pricing (USD per million tokens).
    ///
    // Anthropic list prices, USD per million tokens (verified June 2026):
    // Opus 4.6/4.7/4.8 = $5 in / $25 out, Sonnet 4.x = $3 / $15, Haiku 4.5 = $1 / $5.
    // Cache write = 1.25x input, cache read = 0.10x input. Overridable via pricing.json.
    pub fn seeded() -> Self {
        let opus = Rate::new(5.0, 25.0);
        let sonnet = Rate::new(3.0, 15.0);
        let haiku = Rate::new(1.0, 5.0);
        // OpenAI GPT-5.x public list prices, USD per million tokens.
        // TODO confirm exact gpt-5.x rates; overridable via pricing.json.
        let gpt5 = Rate::new(1.25, 10.0);
        let gpt5_mini = Rate::new(0.25, 2.0);
        let gpt5_nano = Rate::new(0.05, 0.40);
        // DeepSeek (via Reasonix), USD per million tokens. Solved from the real
        // ~/.reasonix/usage.jsonl cost field (input/cache-read/output fit the
        // observed `costUsd` to ~0% error). Cache reads use an EXPLICIT rate
        // because DeepSeek's cache-hit price is ~0.008× input, far from the
        // 0.10× default — using the default would overstate cost by ~40%.
        let deepseek_pro = Rate::with_cache_read(0.435, 0.870, 0.0036);
        let deepseek_flash = Rate::with_cache_read(0.140, 0.280, 0.0012);
        let prefixes = vec![
            ("claude-opus-4-8".to_string(), opus),
            ("claude-opus-4".to_string(), opus),
            ("claude-sonnet-4-6".to_string(), sonnet),
            ("claude-sonnet-4".to_string(), sonnet),
            ("claude-haiku-4-5".to_string(), haiku),
            ("claude-haiku-4".to_string(), haiku),
            // Codex / GPT-5.x. Longest-prefix wins, so the mini/nano variants
            // are matched ahead of the bare `gpt-5` rule.
            ("gpt-5-mini".to_string(), gpt5_mini),
            ("gpt-5-nano".to_string(), gpt5_nano),
            ("gpt-5".to_string(), gpt5),
            // DeepSeek (Reasonix). Longest-prefix wins, so the specific
            // pro/flash variants are matched ahead of the bare `deepseek` rule;
            // `deepseek` itself falls back to the pro rate for any future id.
            ("deepseek-v4-pro".to_string(), deepseek_pro),
            ("deepseek-v4-flash".to_string(), deepseek_flash),
            ("deepseek".to_string(), deepseek_pro),
        ];
        Self {
            rates: HashMap::new(),
            prefixes,
        }
    }

    /// Resolve the rate for a model id: pseudo-model, exact match, then longest
    /// matching prefix.
    pub fn rate(&self, model: &str) -> Option<Rate> {
        if is_pseudo_model(model) {
            return Some(Rate::new(0.0, 0.0));
        }
        if let Some(r) = self.rates.get(model) {
            return Some(*r);
        }
        self.prefixes
            .iter()
            .filter(|(prefix, _)| model.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, r)| *r)
    }

    /// Estimated cost in USD for `usage` under `model`. `None` for unknown models.
    pub fn cost_usd(&self, usage: &Usage, model: &str) -> Option<f64> {
        let rate = self.rate(model)?;
        let cost = (usage.input as f64) * rate.input
            + (usage.output as f64) * rate.output
            + (usage.cache_create as f64) * rate.input * CACHE_CREATE_MULTIPLIER
            + (usage.cache_read as f64) * rate.cache_read_rate();
        Some(cost / PER_MILLION)
    }

    /// Set or override an exact-id rate.
    pub fn set_rate(&mut self, model: &str, input: f64, output: f64) {
        self.rates
            .insert(model.to_string(), Rate::new(input, output));
    }

    /// Serialize the table to pretty JSON (for `pricing.json`).
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parse a table from JSON.
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Fill in any seeded prefix rule this table is MISSING (matched by prefix
    /// string), preserving every existing user override. This lets a persisted
    /// `pricing.json` written before a new model family was added (e.g. DeepSeek)
    /// still resolve a cost for that family, instead of returning `None` until the
    /// user resets the file. User-customized prefixes are never overwritten.
    pub fn merge_seed_defaults(&mut self) {
        let existing: std::collections::HashSet<&str> =
            self.prefixes.iter().map(|(p, _)| p.as_str()).collect();
        let missing: Vec<(String, Rate)> = Self::seeded()
            .prefixes
            .into_iter()
            .filter(|(p, _)| !existing.contains(p.as_str()))
            .collect();
        // Avoid borrowing `self` immutably and mutably at once.
        drop(existing);
        self.prefixes.extend(missing);
    }
}

fn is_pseudo_model(model: &str) -> bool {
    let model = model.trim();
    model.len() >= 2 && model.starts_with('<') && model.ends_with('>')
}
