//! Token → USD cost math.
//!
//! Thin, pure wrappers over the repo's pinned [`ds_models::pricing`] — the
//! same functions the shipped client uses. No re-implementation: a report
//! number is reproducible by recomputing `raw token rows × rates` with the
//! pinned table (README documents the identity). Reasoning is billed as
//! output: the real gateway reports `output_tokens` *including* reasoning,
//! so cost uses `output_tokens` as-is (per the pricing doc).

use crate::usage::UsageRow;

/// USD cost of one usage row for its model, or `None` for unknown models.
pub fn row_cost_usd(row: &UsageRow) -> Option<f64> {
    let rates = ds_models::rates_for_model(&row.model)?;
    Some(ds_models::estimate_cost_usd_from_full_input(
        row.input_tokens,
        row.cache_read_tokens,
        row.output_tokens,
        &rates,
    ))
}

/// Sum of row costs; rows with unknown models are skipped.
pub fn rows_cost_usd(rows: &[UsageRow]) -> f64 {
    rows.iter().filter_map(row_cost_usd).sum()
}

/// Aggregate token tallies over rows.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TokenTotals {
    pub input: u64,
    pub cache_read: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl TokenTotals {
    pub fn from_rows(rows: &[UsageRow]) -> Self {
        rows.iter().fold(Self::default(), |mut t, r| {
            t.input += r.input_tokens;
            t.cache_read += r.cache_read_tokens;
            t.output += r.output_tokens;
            t.reasoning += r.reasoning_tokens;
            t
        })
    }

    /// Aggregate cache-hit fraction (cache_read / input), 0..=1.
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.input == 0 {
            return 0.0;
        }
        (self.cache_read as f64 / self.input as f64).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REAL usage rows captured from live gateway sessions this project
    /// (cache5 chain: `ds -p "Reply with exactly the word: turnN"`, flash).
    /// Kept as a fixture so cost math is tested against actual wire data.
    fn fixture_rows() -> Vec<UsageRow> {
        let mk = |seq, input, cache, output, reasoning| UsageRow {
            seq,
            conv_id: "c".into(),
            req_id: format!("r{seq}"),
            model: "deepseek-v4-flash".into(),
            input_tokens: input,
            cache_read_tokens: cache,
            output_tokens: output,
            reasoning_tokens: reasoning,
            total_tokens: input + output,
        };
        vec![
            mk(1, 9_827, 9_728, 18, 0),
            mk(2, 9_851, 9_728, 3, 0),
            mk(3, 9_875, 9_728, 3, 0),
            mk(4, 9_899, 9_856, 3, 0),
            mk(5, 9_923, 9_856, 3, 0),
        ]
    }

    /// Hand-computed from the pinned flash table
    /// (cache_hit $0.0028/M, cache_miss $0.14/M, output $0.28/M):
    /// row 1: uncached 99 → 99/1e6*0.14 = 0.00001386;
    ///        cached 9728 → 9728/1e6*0.0028 = 0.0000272384;
    ///        out 18 → 18/1e6*0.28 = 0.00000504.
    #[test]
    fn row_cost_matches_hand_computed_flash_rates() {
        let row = &fixture_rows()[0];
        let cost = row_cost_usd(row).expect("flash rates known");
        let expected = 99.0 / 1e6 * 0.14 + 9728.0 / 1e6 * 0.0028 + 18.0 / 1e6 * 0.28;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost} expected={expected}"
        );
    }

    /// Independent recomputation of the whole fixture (the verification-plan
    /// identity: report number == sum of row × rate).
    #[test]
    fn total_cost_equals_independent_recomputation() {
        let rows = fixture_rows();
        let totals = TokenTotals::from_rows(&rows);
        let reported = rows_cost_usd(&rows);
        // Independent recomputation using the raw numbers and the pinned table.
        let rates = ds_models::rates_for_model("deepseek-v4-flash").unwrap();
        let expected = ds_models::estimate_cost_usd_from_full_input(
            totals.input,
            totals.cache_read,
            totals.output,
            &rates,
        );
        assert!(
            (reported - expected).abs() < 1e-9,
            "reported={reported} expected={expected}"
        );
    }

    #[test]
    fn cache_hit_ratio_matches_captured_trajectory() {
        let rows = fixture_rows();
        let totals = TokenTotals::from_rows(&rows);
        let ratio = totals.cache_hit_ratio();
        let input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        let cache: u64 = rows.iter().map(|r| r.cache_read_tokens).sum();
        assert!((ratio - (cache as f64 / input as f64)).abs() < 1e-9);
        // Captured real trajectory was 98.5–99.6% per turn.
        assert!(ratio > 0.98 && ratio < 1.0, "ratio={ratio}");
    }

    #[test]
    fn unknown_model_yields_none() {
        let row = UsageRow {
            seq: 1,
            conv_id: "c".into(),
            req_id: "r".into(),
            model: "gpt-4o".into(),
            input_tokens: 100,
            cache_read_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            total_tokens: 110,
        };
        assert!(row_cost_usd(&row).is_none());
    }
}
