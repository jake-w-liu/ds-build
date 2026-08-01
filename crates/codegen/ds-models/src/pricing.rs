//! DeepSeek API list prices (USD per 1M tokens).
//!
//! Source: https://api-docs.deepseek.com/quick_start/pricing/
//! Rates are pinned here so cost estimates and unit tests stay stable when the
//! live pricing page changes. Label UI output as an estimate when not using
//! wire-reported `cost_in_usd_ticks`.
//!
//! Peak/off-peak multipliers are intentionally omitted until DeepSeek's
//! announced effective date (see DEEPSEEK.md / pricing page notes).

/// USD charged per one million tokens for one billing category.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeepSeekTokenRates {
    /// Cache-hit input (disk prefix cache read).
    pub cache_hit_per_mtok: f64,
    /// Cache-miss input (uncached prompt tokens).
    pub cache_miss_per_mtok: f64,
    /// Output tokens (includes reasoning tokens billed as output).
    pub output_per_mtok: f64,
}

/// deepseek-v4-flash list prices (regular / non-peak).
pub const DEEPSEEK_V4_FLASH_RATES: DeepSeekTokenRates = DeepSeekTokenRates {
    cache_hit_per_mtok: 0.0028,
    cache_miss_per_mtok: 0.14,
    output_per_mtok: 0.28,
};

/// deepseek-v4-pro list prices (regular / non-peak).
pub const DEEPSEEK_V4_PRO_RATES: DeepSeekTokenRates = DeepSeekTokenRates {
    cache_hit_per_mtok: 0.003625,
    cache_miss_per_mtok: 0.435,
    output_per_mtok: 0.87,
};

/// Resolve list rates for a model id (slug or common aliases).
///
/// Returns `None` for unknown models so callers can skip estimates rather than
/// invent a wrong price.
pub fn rates_for_model(model_id: &str) -> Option<DeepSeekTokenRates> {
    let id = model_id.trim().to_ascii_lowercase();
    // Match flash before pro so "deepseek-v4-flash" does not hit a loose pro check.
    if id.contains("flash") {
        return Some(DEEPSEEK_V4_FLASH_RATES);
    }
    if id.contains("pro") || id == "deepseek-v4-pro" || id.starts_with("deepseek-v4-pro") {
        return Some(DEEPSEEK_V4_PRO_RATES);
    }
    // Bare / historical aliases that default to pro in this product.
    if id == "deepseek-chat" || id == "deepseek-reasoner" {
        return Some(DEEPSEEK_V4_PRO_RATES);
    }
    None
}

/// Estimate USD cost from a token breakdown using DeepSeek list rates.
///
/// - `uncached_input_tokens`: cache-miss portion of the prompt
/// - `cached_input_tokens`: cache-hit / cache-read portion
/// - `output_tokens`: completion tokens (reasoning is billed as output)
///
/// Does not apply peak multipliers. Pure arithmetic — no I/O.
pub fn estimate_cost_usd(
    uncached_input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    rates: &DeepSeekTokenRates,
) -> f64 {
    const M: f64 = 1_000_000.0;
    let uncached = uncached_input_tokens as f64 / M * rates.cache_miss_per_mtok;
    let cached = cached_input_tokens as f64 / M * rates.cache_hit_per_mtok;
    let output = output_tokens as f64 / M * rates.output_per_mtok;
    uncached + cached + output
}

/// Estimate from full prompt size + cache-read subset (ds-build ledger identity).
///
/// `full_input_tokens` is the full prompt (uncached + cache reads). Cached tokens
/// are subtracted saturatingly so bad wire data cannot overflow.
pub fn estimate_cost_usd_from_full_input(
    full_input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    rates: &DeepSeekTokenRates,
) -> f64 {
    let uncached = full_input_tokens.saturating_sub(cached_input_tokens);
    estimate_cost_usd(uncached, cached_input_tokens, output_tokens, rates)
}

/// Convenience: estimate for a model id, or `None` if the model is unknown.
pub fn estimate_cost_usd_for_model(
    model_id: &str,
    full_input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
) -> Option<f64> {
    let rates = rates_for_model(model_id)?;
    Some(estimate_cost_usd_from_full_input(
        full_input_tokens,
        cached_input_tokens,
        output_tokens,
        &rates,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_and_pro_rates_match_official_table() {
        // Pinned to api-docs.deepseek.com Models & Pricing (regular rates).
        assert_eq!(DEEPSEEK_V4_FLASH_RATES.cache_hit_per_mtok, 0.0028);
        assert_eq!(DEEPSEEK_V4_FLASH_RATES.cache_miss_per_mtok, 0.14);
        assert_eq!(DEEPSEEK_V4_FLASH_RATES.output_per_mtok, 0.28);
        assert_eq!(DEEPSEEK_V4_PRO_RATES.cache_hit_per_mtok, 0.003625);
        assert_eq!(DEEPSEEK_V4_PRO_RATES.cache_miss_per_mtok, 0.435);
        assert_eq!(DEEPSEEK_V4_PRO_RATES.output_per_mtok, 0.87);
    }

    #[test]
    fn rates_for_model_resolves_flash_and_pro_slugs() {
        assert_eq!(
            rates_for_model("deepseek-v4-flash"),
            Some(DEEPSEEK_V4_FLASH_RATES)
        );
        assert_eq!(
            rates_for_model("DeepSeek-V4-Flash"),
            Some(DEEPSEEK_V4_FLASH_RATES)
        );
        assert_eq!(
            rates_for_model("deepseek-v4-pro"),
            Some(DEEPSEEK_V4_PRO_RATES)
        );
        assert_eq!(
            rates_for_model("deepseek-v4-pro.5"),
            Some(DEEPSEEK_V4_PRO_RATES)
        );
        assert!(rates_for_model("gpt-4o").is_none());
    }

    #[test]
    fn estimate_cost_usd_flash_known_breakdown() {
        // 900k uncached + 100k cache-hit + 50k output on Flash.
        // miss: 900_000/1e6 * 0.14 = 0.126
        // hit:  100_000/1e6 * 0.0028 = 0.00028
        // out:   50_000/1e6 * 0.28 = 0.014
        let cost = estimate_cost_usd(900_000, 100_000, 50_000, &DEEPSEEK_V4_FLASH_RATES);
        let expected = 0.126 + 0.00028 + 0.014;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost} expected={expected}"
        );
    }

    #[test]
    fn estimate_cost_usd_pro_from_full_input() {
        // full=1_000_000, cached=250_000 → uncached=750_000; out=10_000
        // miss: 0.75 * 0.435 = 0.32625
        // hit:  0.25 * 0.003625 = 0.00090625
        // out:  0.01 * 0.87 = 0.0087
        let cost =
            estimate_cost_usd_from_full_input(1_000_000, 250_000, 10_000, &DEEPSEEK_V4_PRO_RATES);
        let expected = 0.32625 + 0.00090625 + 0.0087;
        assert!((cost - expected).abs() < 1e-12, "cost={cost} expected={expected}");
    }

    #[test]
    fn estimate_for_model_drives_shipped_helper() {
        let cost = estimate_cost_usd_for_model("deepseek-v4-flash", 1_000_000, 0, 0).unwrap();
        assert!((cost - 0.14).abs() < 1e-12);
        assert!(estimate_cost_usd_for_model("unknown-model", 1, 0, 0).is_none());
    }

    #[test]
    fn pure_cache_hit_bills_hit_rate_only() {
        let cost = estimate_cost_usd(0, 1_000_000, 0, &DEEPSEEK_V4_FLASH_RATES);
        assert!((cost - 0.0028).abs() < 1e-12);
    }
}
