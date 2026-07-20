//! Best-effort API-equivalent pricing for subscription usage without a cost.
//!
//! Rates are standard text-token prices per one million tokens, verified
//! against the OpenAI model documentation on 2026-07-21. Estimates deliberately
//! exclude tool-call fees, regional/priority uplifts, and long-context
//! multipliers that cannot be reconstructed reliably from aggregate usage.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ApiPrice {
    input: f64,
    cached_input: f64,
    output: f64,
    cache_write: f64,
}

impl ApiPrice {
    const fn new(input: f64, cached_input: f64, output: f64) -> Self {
        Self {
            input,
            cached_input,
            output,
            cache_write: input,
        }
    }

    const fn with_cache_write(mut self, cache_write: f64) -> Self {
        self.cache_write = cache_write;
        self
    }

    fn estimate(self, tokens: TokenCounts) -> f64 {
        (tokens.input as f64 * self.input
            + tokens.cache_read as f64 * self.cached_input
            + tokens.output as f64 * self.output
            + tokens.cache_write as f64 * self.cache_write)
            / 1_000_000.0
    }
}

pub fn estimate_api_cost(provider: &str, model: &str, tokens: TokenCounts) -> Option<f64> {
    if !provider.eq_ignore_ascii_case("openai") {
        return None;
    }
    openai_price(model).map(|price| price.estimate(tokens))
}

fn openai_price(model: &str) -> Option<ApiPrice> {
    let model = model.trim().to_ascii_lowercase();
    let model = model.as_str();

    if named_or_snapshot(model, "gpt-5.6-sol") || named_or_snapshot(model, "gpt-5.6") {
        Some(ApiPrice::new(5.0, 0.5, 30.0).with_cache_write(6.25))
    } else if named_or_snapshot(model, "gpt-5.6-terra") {
        Some(ApiPrice::new(2.5, 0.25, 15.0).with_cache_write(3.125))
    } else if named_or_snapshot(model, "gpt-5.6-luna") {
        Some(ApiPrice::new(1.0, 0.1, 6.0).with_cache_write(1.25))
    } else if named_or_snapshot(model, "gpt-5.5-pro") {
        Some(ApiPrice::new(30.0, 30.0, 180.0))
    } else if named_or_snapshot(model, "gpt-5.5") {
        Some(ApiPrice::new(5.0, 0.5, 30.0))
    } else if named_or_snapshot(model, "gpt-5.4-pro") {
        Some(ApiPrice::new(30.0, 30.0, 180.0))
    } else if named_or_snapshot(model, "gpt-5.4-mini") {
        Some(ApiPrice::new(0.75, 0.075, 4.5))
    } else if named_or_snapshot(model, "gpt-5.4-nano") {
        Some(ApiPrice::new(0.2, 0.02, 1.25))
    } else if named_or_snapshot(model, "gpt-5.4") {
        Some(ApiPrice::new(2.5, 0.25, 15.0))
    } else if named_or_snapshot(model, "gpt-5.3-codex")
        || named_or_snapshot(model, "gpt-5.3-chat-latest")
        || named_or_snapshot(model, "gpt-5.2-codex")
        || named_or_snapshot(model, "gpt-5.2")
    {
        Some(ApiPrice::new(1.75, 0.175, 14.0))
    } else if named_or_snapshot(model, "gpt-5.1-codex-mini") {
        Some(ApiPrice::new(0.25, 0.025, 2.0))
    } else if named_or_snapshot(model, "gpt-5.1-codex-max")
        || named_or_snapshot(model, "gpt-5.1-codex")
        || named_or_snapshot(model, "gpt-5.1-chat-latest")
        || named_or_snapshot(model, "gpt-5.1")
        || named_or_snapshot(model, "gpt-5-codex")
        || named_or_snapshot(model, "gpt-5-chat-latest")
        || named_or_snapshot(model, "gpt-5")
    {
        Some(ApiPrice::new(1.25, 0.125, 10.0))
    } else if model == "codex-mini-latest" {
        Some(ApiPrice::new(1.5, 0.375, 6.0))
    } else {
        None
    }
}

fn named_or_snapshot(model: &str, name: &str) -> bool {
    model == name
        || model
            .strip_prefix(name)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .is_some_and(|suffix| suffix.starts_with(|character: char| character.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_documented_openai_token_categories() {
        let cost = estimate_api_cost(
            "openai",
            "gpt-5.6-sol",
            TokenCounts {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: 1_000_000,
                cache_write: 1_000_000,
            },
        )
        .unwrap();

        assert_eq!(cost, 41.75);
    }

    #[test]
    fn recognizes_snapshots_without_guessing_unpublished_variants() {
        assert!(openai_price("gpt-5.5-2026-04-23").is_some());
        assert!(openai_price("gpt-5.3-codex").is_some());
        assert!(openai_price("gpt-5.3-codex-spark").is_none());
        assert!(openai_price("some-future-model").is_none());
    }

    #[test]
    fn ignores_non_openai_providers() {
        assert_eq!(
            estimate_api_cost("openrouter", "gpt-5.6-sol", TokenCounts::default()),
            None
        );
    }
}
