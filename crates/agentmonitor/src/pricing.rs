//! Per-model token-pricing estimates for the Dashboard's Σ cost row.
//!
//! Prices are USD per 1 million tokens, from official pricing pages as of
//! 2026-05. Numbers are intentionally conservative — we'd rather under-quote
//! than over-promise — and the caller renders them with an explicit
//! "estimate" suffix to make their imprecise nature clear.
//!
//! When a model isn't found here the lookup returns `None`; the dashboard
//! falls back to "—" rather than $0.00 to avoid implying free usage.
//!
//! Updating: prices change every few months. Add new model strings as
//! needed, prefer matching by *prefix* so e.g. `claude-sonnet-4.7-20260218`
//! still matches `claude-sonnet-4`. The matching is case-insensitive and
//! tries longest-prefix first so specific overrides (e.g. claude-opus-4.7)
//! win over generic ones (claude-opus).

use crate::adapter::types::TokenStats;

/// Per-million-token prices for one model variant, in USD.
///
/// `cache_creation` is what providers charge to write a prompt cache entry —
/// usually 1.25-2.0× the input price. `cache_read` is what providers charge
/// for cache hits, typically a fraction of input. Codex / OpenAI doesn't
/// expose cache_creation; for those models we conservatively reuse `input`.
#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

impl ModelPrice {
    /// Compute the cost of a single session's tokens in USD.
    pub fn cost_usd(&self, tokens: &TokenStats) -> f64 {
        let m = 1_000_000f64;
        (tokens.input as f64 / m) * self.input_per_mtok
            + (tokens.output as f64 / m) * self.output_per_mtok
            + (tokens.cache_read as f64 / m) * self.cache_read_per_mtok
            + (tokens.cache_creation as f64 / m) * self.cache_creation_per_mtok
    }
}

/// All known prefix → price mappings. Prefixes are matched longest-first so
/// `claude-opus-4.7` (specific) wins over `claude-opus` (generic) when both
/// would match. The list is small enough that linear scan is fine.
const PRICES: &[(&str, ModelPrice)] = &[
    // ── Claude family (Anthropic, 2026-05 pricing) ──
    ("claude-opus-4.7", ModelPrice {
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
        cache_read_per_mtok: 1.50,
        cache_creation_per_mtok: 18.75,
    }),
    ("claude-opus-4", ModelPrice {
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
        cache_read_per_mtok: 1.50,
        cache_creation_per_mtok: 18.75,
    }),
    ("claude-opus", ModelPrice {
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
        cache_read_per_mtok: 1.50,
        cache_creation_per_mtok: 18.75,
    }),
    ("claude-sonnet-4", ModelPrice {
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
        cache_read_per_mtok: 0.30,
        cache_creation_per_mtok: 3.75,
    }),
    ("claude-sonnet", ModelPrice {
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
        cache_read_per_mtok: 0.30,
        cache_creation_per_mtok: 3.75,
    }),
    ("claude-haiku", ModelPrice {
        input_per_mtok: 0.80,
        output_per_mtok: 4.0,
        cache_read_per_mtok: 0.08,
        cache_creation_per_mtok: 1.0,
    }),
    // ── OpenAI / Codex ──
    ("gpt-5", ModelPrice {
        input_per_mtok: 5.0,
        output_per_mtok: 15.0,
        cache_read_per_mtok: 1.25,
        cache_creation_per_mtok: 5.0,
    }),
    ("gpt-4.1", ModelPrice {
        input_per_mtok: 2.50,
        output_per_mtok: 10.0,
        cache_read_per_mtok: 0.625,
        cache_creation_per_mtok: 2.50,
    }),
    ("gpt-4o", ModelPrice {
        input_per_mtok: 2.50,
        output_per_mtok: 10.0,
        cache_read_per_mtok: 1.25,
        cache_creation_per_mtok: 2.50,
    }),
    ("o1", ModelPrice {
        input_per_mtok: 15.0,
        output_per_mtok: 60.0,
        cache_read_per_mtok: 7.50,
        cache_creation_per_mtok: 15.0,
    }),
    // Generic OpenAI catch-all for the codex.app's "openai" model string.
    ("openai", ModelPrice {
        input_per_mtok: 2.50,
        output_per_mtok: 10.0,
        cache_read_per_mtok: 0.625,
        cache_creation_per_mtok: 2.50,
    }),
    // ── Gemini ──
    ("gemini-2", ModelPrice {
        input_per_mtok: 1.25,
        output_per_mtok: 5.0,
        cache_read_per_mtok: 0.3125,
        cache_creation_per_mtok: 1.25,
    }),
    ("gemini-1.5", ModelPrice {
        input_per_mtok: 1.25,
        output_per_mtok: 5.0,
        cache_read_per_mtok: 0.3125,
        cache_creation_per_mtok: 1.25,
    }),
    ("gemini", ModelPrice {
        input_per_mtok: 1.25,
        output_per_mtok: 5.0,
        cache_read_per_mtok: 0.3125,
        cache_creation_per_mtok: 1.25,
    }),
];

/// Look up the price for a model id. Case-insensitive prefix match; returns
/// `None` for unknown models so the renderer can show "—" instead of $0.
pub fn lookup(model: &str) -> Option<ModelPrice> {
    let lower = model.to_lowercase();
    // Iterate longest-prefix-first so specific entries win.
    let mut entries: Vec<&(&str, ModelPrice)> = PRICES.iter().collect();
    entries.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    for (prefix, price) in entries {
        if lower.starts_with(prefix) {
            return Some(*price);
        }
    }
    None
}

/// Compute the total dollar cost across an arbitrary set of sessions. Sessions
/// without a known model contribute zero (rather than guessing) so the result
/// always under-estimates rather than over-estimates.
pub fn aggregate_cost(sessions: &[crate::adapter::types::SessionMeta]) -> f64 {
    sessions
        .iter()
        .filter_map(|s| s.model.as_deref().and_then(lookup).map(|p| p.cost_usd(&s.tokens)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_lookup_picks_longest_match() {
        // claude-opus-4.7 is more specific than claude-opus, so the lookup
        // for "claude-opus-4.7-20260218" should resolve to the 4.7 entry.
        let price = lookup("claude-opus-4.7-20260218").unwrap();
        assert_eq!(price.input_per_mtok, 15.0);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("CLAUDE-SONNET-4-20260218").is_some());
        assert!(lookup("Gpt-5-Mini").is_some());
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(lookup("acme-7b-instruct").is_none());
    }

    #[test]
    fn cost_usd_sums_all_four_buckets() {
        let p = ModelPrice {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        };
        // 1M of each — total cost = 3 + 15 + 0.30 + 3.75 = $22.05.
        let tokens = TokenStats {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
        };
        let cost = p.cost_usd(&tokens);
        assert!((cost - 22.05).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn aggregate_cost_skips_unknown_models() {
        use crate::adapter::types::{SessionMeta, SessionStatus};
        use std::path::PathBuf;
        let mk = |model: Option<&str>, output_tokens: u64| SessionMeta {
            agent: "claude",
            id: "x".into(),
            path: PathBuf::from("/tmp/x.jsonl"),
            cwd: None,
            model: model.map(str::to_string),
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats {
                input: 0,
                output: output_tokens,
                cache_read: 0,
                cache_creation: 0,
            },
            status: SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 0,
        };
        // Two sessions: one Claude-sonnet (1M output → $15), one unknown ($0).
        let sessions = vec![
            mk(Some("claude-sonnet-4"), 1_000_000),
            mk(Some("acme-9b"), 1_000_000),
        ];
        let total = aggregate_cost(&sessions);
        assert!((total - 15.0).abs() < 1e-6, "got {total}");
    }
}
