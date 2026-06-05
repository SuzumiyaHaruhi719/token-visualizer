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
}

impl Rate {
    pub const fn new(input: f64, output: f64) -> Self {
        Self { input, output }
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
    // TODO: confirm exact $/M from Anthropic pricing page. These are seed values;
    // they are config-overridable via `pricing.json`, so correctness here is data,
    // not code. Current seeds: Opus 4.x = $15 in / $75 out, Sonnet 4.x = $3 / $15,
    // Haiku 4.x = $1 / $5.
    pub fn seeded() -> Self {
        let opus = Rate::new(15.0, 75.0);
        let sonnet = Rate::new(3.0, 15.0);
        let haiku = Rate::new(1.0, 5.0);
        let prefixes = vec![
            ("claude-opus-4-8".to_string(), opus),
            ("claude-opus-4".to_string(), opus),
            ("claude-sonnet-4-6".to_string(), sonnet),
            ("claude-sonnet-4".to_string(), sonnet),
            ("claude-haiku-4-5".to_string(), haiku),
            ("claude-haiku-4".to_string(), haiku),
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
            + (usage.cache_read as f64) * rate.input * CACHE_READ_MULTIPLIER;
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
}

fn is_pseudo_model(model: &str) -> bool {
    let model = model.trim();
    model.len() >= 2 && model.starts_with('<') && model.ends_with('>')
}
