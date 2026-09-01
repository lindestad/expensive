//! Best-effort API-equivalent pricing for subscription and provider usage without a cost.
//!
//! OpenAI rates are standard text-token prices per one million tokens, verified
//! against the OpenAI model documentation on 2026-07-21.
//!
//! Claude rates are standard text-token prices per one million tokens, verified
//! against Claude pricing documentation (https://platform.claude.com/docs/en/about-claude/pricing) on 2026-09-01.
//!
//! Bedrock estimates are standard on-demand retail/API-equivalent comparisons,
//! not AWS invoices. They deliberately exclude regional uplifts, priority/flex
//! tiers, discounts, provisioned throughput, custom models, application
//! inference profiles, gateways, tool-call fees, long-context multipliers, and
//! billing adjustments.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl TokenCounts {
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ApiPrice {
    input: f64,
    cached_input: f64,
    output: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
}

impl ApiPrice {
    const fn new(input: f64, cached_input: f64, output: f64) -> Self {
        Self {
            input,
            cached_input,
            output,
            cache_write_5m: input,
            cache_write_1h: input,
        }
    }

    const fn with_cache_write(mut self, cache_write: f64) -> Self {
        self.cache_write_5m = cache_write;
        self.cache_write_1h = cache_write;
        self
    }

    const fn new_claude(
        input: f64,
        cache_write_5m: f64,
        cache_write_1h: f64,
        cached_input: f64,
        output: f64,
    ) -> Self {
        Self {
            input,
            cached_input,
            output,
            cache_write_5m,
            cache_write_1h,
        }
    }

    fn estimate(self, tokens: TokenCounts) -> f64 {
        (tokens.input as f64 * self.input
            + tokens.cache_read as f64 * self.cached_input
            + tokens.output as f64 * self.output
            + tokens.cache_write_5m as f64 * self.cache_write_5m
            + tokens.cache_write_1h as f64 * self.cache_write_1h)
            / 1_000_000.0
    }
}

pub fn should_estimate(provider: &str, estimate_api_cost_flag: bool) -> bool {
    if provider.eq_ignore_ascii_case("amazon-bedrock") {
        true
    } else if provider.eq_ignore_ascii_case("anthropic") || provider.eq_ignore_ascii_case("openai")
    {
        estimate_api_cost_flag
    } else {
        false
    }
}

pub fn estimate_cost(provider: &str, model: &str, tokens: TokenCounts) -> Option<f64> {
    if provider.eq_ignore_ascii_case("openai") {
        openai_price(model).map(|price| price.estimate(tokens))
    } else if provider.eq_ignore_ascii_case("anthropic")
        || provider.eq_ignore_ascii_case("amazon-bedrock")
    {
        claude_price(model).map(|price| price.estimate(tokens))
    } else {
        None
    }
}

pub fn estimate_api_cost(provider: &str, model: &str, tokens: TokenCounts) -> Option<f64> {
    estimate_cost(provider, model, tokens)
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

fn normalize_claude_model(raw_model: &str) -> Option<&str> {
    let mut model = raw_model.trim();
    if model.starts_with("arn:") {
        // Exclude custom, application, provisioned, or opaque ARNs
        if model.contains(":application-inference-profile/")
            || model.contains(":custom-model/")
            || model.contains(":provisioned-model/")
        {
            return None;
        }
        if !model.starts_with("arn:aws:bedrock:")
            && !model.starts_with("arn:aws-us-gov:bedrock:")
            && !model.starts_with("arn:aws-cn:bedrock:")
        {
            return None;
        }
        if !model.contains(":foundation-model/") && !model.contains(":inference-profile/") {
            return None;
        }
        model = match model.rsplit_once('/') {
            Some((_, suffix)) => suffix,
            None => return None,
        };
    }

    let model = if let Some(rest) = model.strip_prefix("anthropic.") {
        rest
    } else if let Some(rest) = model.strip_prefix("us.anthropic.") {
        rest
    } else if let Some(rest) = model.strip_prefix("eu.anthropic.") {
        rest
    } else if let Some(rest) = model.strip_prefix("apac.anthropic.") {
        rest
    } else if let Some(rest) = model.strip_prefix("global.anthropic.") {
        rest
    } else {
        if model.contains("anthropic.") {
            return None;
        }
        model
    };

    Some(model)
}

fn claude_price(raw_model: &str) -> Option<ApiPrice> {
    let model = normalize_claude_model(raw_model)?.to_ascii_lowercase();
    let model = model.as_str();

    if matches_model_prefix(
        model,
        &[
            "claude-fable-5",
            "claude-5-fable",
            "fable-5",
            "claude-mythos-5",
            "claude-5-mythos",
            "mythos-5",
        ],
    ) {
        Some(ApiPrice::new_claude(10.0, 12.50, 20.0, 1.0, 50.0))
    } else if matches_model_prefix(
        model,
        &[
            "claude-opus-5",
            "claude-5-opus",
            "opus-5",
            "claude-opus-4-8",
            "claude-opus-4.8",
            "opus-4-8",
            "opus-4.8",
            "claude-opus-4-7",
            "claude-opus-4.7",
            "opus-4-7",
            "opus-4.7",
            "claude-opus-4-6",
            "claude-opus-4.6",
            "opus-4-6",
            "opus-4.6",
            "claude-opus-4-5",
            "claude-opus-4.5",
            "opus-4-5",
            "opus-4.5",
        ],
    ) {
        Some(ApiPrice::new_claude(5.0, 6.25, 10.0, 0.50, 25.0))
    } else if matches_model_prefix(
        model,
        &[
            "claude-opus-4-1",
            "claude-opus-4.1",
            "opus-4-1",
            "opus-4.1",
            "claude-opus-4-0",
            "claude-opus-4.0",
            "claude-opus-4",
            "claude-4-opus",
            "opus-4-0",
            "opus-4.0",
            "opus-4",
        ],
    ) {
        Some(ApiPrice::new_claude(15.0, 18.75, 30.0, 1.50, 75.0))
    } else if matches_model_prefix(
        model,
        &[
            "claude-sonnet-5",
            "claude-5-sonnet",
            "sonnet-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4.6",
            "sonnet-4-6",
            "sonnet-4.6",
            "claude-sonnet-4-5",
            "claude-sonnet-4.5",
            "sonnet-4-5",
            "sonnet-4.5",
            "claude-sonnet-4-0",
            "claude-sonnet-4.0",
            "claude-sonnet-4",
            "claude-4-sonnet",
            "sonnet-4-0",
            "sonnet-4.0",
            "sonnet-4",
        ],
    ) {
        Some(ApiPrice::new_claude(3.0, 3.75, 6.0, 0.30, 15.0))
    } else if matches_model_prefix(
        model,
        &[
            "claude-haiku-4-5",
            "claude-haiku-4.5",
            "claude-4-5-haiku",
            "claude-4.5-haiku",
            "haiku-4-5",
            "haiku-4.5",
        ],
    ) {
        Some(ApiPrice::new_claude(1.0, 1.25, 2.0, 0.10, 5.0))
    } else if matches_model_prefix(
        model,
        &[
            "claude-haiku-3-5",
            "claude-haiku-3.5",
            "claude-3-5-haiku",
            "claude-3.5-haiku",
            "haiku-3-5",
            "haiku-3.5",
        ],
    ) {
        Some(ApiPrice::new_claude(0.80, 1.0, 1.60, 0.08, 4.0))
    } else {
        None
    }
}

fn matches_model_prefix(model: &str, names: &[&str]) -> bool {
    for &name in names {
        if model == name {
            return true;
        }
        if let Some(suffix) = model.strip_prefix(name) {
            if is_valid_claude_suffix(suffix) {
                return true;
            }
        }
    }
    false
}

fn is_valid_bedrock_version(s: &str) -> bool {
    let rest = match s.strip_prefix('v') {
        Some(r) => r,
        None => return false,
    };
    if let Some((major, minor)) = rest.split_once(':') {
        !major.is_empty()
            && major.chars().all(|c| c.is_ascii_digit())
            && !minor.is_empty()
            && minor.chars().all(|c| c.is_ascii_digit())
    } else {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    }
}

fn is_valid_claude_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }
    if let Some(rest) = suffix.strip_prefix(':') {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    let rest = if let Some(s) = suffix.strip_prefix('-') {
        s
    } else {
        return false;
    };
    if is_valid_bedrock_version(rest) {
        return true;
    }
    let (prefix, after_prefix) = if let Some((date, after)) = rest.split_once('-') {
        (date, Some(after))
    } else if let Some((date, after)) = rest.split_once(':') {
        if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            (date, None)
        } else {
            return false;
        }
    } else {
        (rest, None)
    };

    if prefix.len() == 8 && prefix.chars().all(|c| c.is_ascii_digit()) {
        if let Some(second) = after_prefix {
            return is_valid_bedrock_version(second);
        }
        return true;
    }
    false
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
                cache_write_5m: 1_000_000,
                cache_write_1h: 0,
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
    fn estimates_claude_model_families_with_cache_durations() {
        // Fable 5: 10 + 12.5 + 20 + 1 + 50 = 93.5
        let cost = estimate_cost(
            "anthropic",
            "claude-fable-5",
            TokenCounts {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: 1_000_000,
                cache_write_5m: 1_000_000,
                cache_write_1h: 1_000_000,
            },
        )
        .unwrap();
        assert!((cost - 93.5).abs() < 1e-6);

        // Sonnet 5: 3 + 3.75 + 6 + 0.3 + 15 = 28.05
        let cost = estimate_cost(
            "anthropic",
            "claude-sonnet-5",
            TokenCounts {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: 1_000_000,
                cache_write_5m: 1_000_000,
                cache_write_1h: 1_000_000,
            },
        )
        .unwrap();
        assert!((cost - 28.05).abs() < 1e-6);

        // Sonnet 4.5
        let cost = estimate_cost(
            "amazon-bedrock",
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            TokenCounts {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: 1_000_000,
                cache_write_5m: 1_000_000,
                cache_write_1h: 1_000_000,
            },
        )
        .unwrap();
        // Sonnet 4.5: 3 + 3.75 + 6 + 0.3 + 15 = 28.05
        assert!((cost - 28.05).abs() < 1e-6);
    }

    #[test]
    fn recognizes_canonical_dotted_aliases() {
        assert!(claude_price("claude-opus-4.8").is_some());
        assert!(claude_price("opus-4.8").is_some());
        assert!(claude_price("claude-sonnet-4.6").is_some());
        assert!(claude_price("claude-haiku-4.5").is_some());
        assert!(claude_price("claude-opus-4.7").is_some());
        assert!(claude_price("claude-opus-4.1").is_some());
        assert!(claude_price("claude-sonnet-4.5").is_some());
        assert!(claude_price("claude-haiku-3.5").is_some());
    }

    #[test]
    fn leaves_opaque_arns_and_unknown_models_unpriced() {
        assert!(
            claude_price("arn:aws:bedrock:us-east-1:123456789012:inference-profile/custom")
                .is_none()
        );
        assert!(claude_price(
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-app"
        )
        .is_none());
        assert!(claude_price(
            "arn:aws:bedrock:us-east-1:123456789012:custom-model/anthropic.claude-3-5-sonnet"
        )
        .is_none());
        assert!(claude_price(
            "arn:aws:bedrock:us-east-1:123456789012:provisioned-model/anthropic.claude-3-5-sonnet"
        )
        .is_none());
        assert!(claude_price("<synthetic>").is_none());
        assert!(claude_price("unknown-model").is_none());
        assert!(claude_price("claude-sonnet-4.7").is_none());
        assert!(claude_price("claude-sonnet-4-7").is_none());
        assert!(claude_price("claude-opus-4.9").is_none());
        assert!(claude_price("claude-opus-4-9").is_none());
        assert!(claude_price("claude-3-5-sonnet").is_none());
        assert!(claude_price("claude-3-7-sonnet").is_none());
        assert!(claude_price("claude-3.5-sonnet").is_none());
        assert!(claude_price("claude-3.7-sonnet").is_none());
        assert!(claude_price("us.anthropic.claude-3-5-sonnet-20241022-v2:0").is_none());
        assert!(claude_price("us.anthropic.claude-3-7-sonnet-20250219-v1:0").is_none());
        // Lookalikes rejected
        assert!(claude_price("claude-sonnet-5-v1beta").is_none());
        assert!(claude_price("claude-sonnet-5-20250929-v1beta").is_none());
        // Malformed colon cases rejected
        assert!(claude_price("claude-sonnet-5:garbage").is_none());
        assert!(claude_price("claude-sonnet-5-v1:garbage").is_none());
        assert!(claude_price("claude-sonnet-5-20250929:garbage").is_none());
        assert!(claude_price("claude-sonnet-5-20250929-v1:garbage").is_none());
        // Arbitrary prefix containing anthropic. rejected
        assert!(claude_price("custom.anthropic.claude-sonnet-5").is_none());
    }

    #[test]
    fn recognizes_standard_bedrock_arns_and_regions() {
        assert!(claude_price(
            "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-5-20250929-v1:0"
        )
        .is_some());
        assert!(claude_price("arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-sonnet-5-20250929-v1:0").is_some());
        assert!(claude_price("eu.anthropic.claude-sonnet-5-20250929-v1:0").is_some());
        assert!(claude_price("apac.anthropic.claude-sonnet-4-5-20250929-v1:0").is_some());
        assert!(claude_price("global.anthropic.claude-haiku-4-5-v1:0").is_some());
    }

    #[test]
    fn checks_should_estimate_rules() {
        assert!(should_estimate("amazon-bedrock", false));
        assert!(should_estimate("amazon-bedrock", true));
        assert!(!should_estimate("anthropic", false));
        assert!(should_estimate("anthropic", true));
        assert!(!should_estimate("openai", false));
        assert!(should_estimate("openai", true));
        assert!(!should_estimate("github-copilot", true));
    }
}
