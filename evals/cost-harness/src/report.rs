//! Report model + rendering (JSON and human Markdown).
//!
//! Every number in a report is reproducible from the raw `usage.jsonl` rows ×
//! the pinned `ds-models` rates (see README "Cost identity").

use serde::{Deserialize, Serialize};

use crate::cost::{TokenTotals, rows_cost_usd};
use crate::usage::UsageRow;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioReport {
    pub scenario: String,
    pub mode: String,
    pub headroom: String, // "on" | "off"
    pub pass: bool,
    pub runs: Vec<RunReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub run_index: usize,
    /// headroom state this run used: Some(true)=ON, Some(false)=OFF,
    /// None = default (on).
    pub headroom: Option<bool>,
    pub rows: Vec<UsageRow>,
    pub totals: TokenTotals,
    pub cost_usd: f64,
    pub cache_hit_pct: f64,
    pub markers_ok: bool,
    pub wire_assertions: Vec<String>,
    pub gate_assertions: Vec<String>,
    pub compaction_fired: bool,
    pub compression_reduction_pct: Option<f64>,
    pub failure_notes: Vec<String>,
}

impl RunReport {
    pub fn passed(&self) -> bool {
        self.markers_ok
            && self.wire_assertions.is_empty()
            && self.gate_assertions.is_empty()
            && self.failure_notes.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavingsReport {
    pub on_cost_usd: Option<f64>,
    pub off_cost_usd: Option<f64>,
    pub delta_usd: Option<f64>,
    pub on_pass: bool,
    pub off_pass: bool,
    pub compression_reduction_pct: Option<f64>,
}

impl SavingsReport {
    /// ON-mode measured cost ≤ OFF-mode, both recorded (acceptance 4).
    pub fn saves_money(&self) -> bool {
        match (self.on_cost_usd, self.off_cost_usd) {
            (Some(on), Some(off)) => on <= off,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HarnessReport {
    pub mode: String,
    pub ds_bin: String,
    pub created_at: String,
    pub scenarios: Vec<ScenarioReport>,
    pub savings: Option<SavingsReport>,
}

impl HarnessReport {
    pub fn all_scenarios_pass(&self) -> bool {
        self.scenarios.iter().all(|s| s.pass)
    }

    /// Every run's `pass` flag across all scenarios.
    pub fn passes(&self) -> Vec<(String, String, bool)> {
        self.scenarios
            .iter()
            .flat_map(|s| s.runs.iter().map(move |r| (s.scenario.clone(), s.headroom.clone(), r.passed())))
            .collect()
    }

    pub fn render_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serializes")
    }

    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# DS Cost Harness report\n\n");
        out.push_str(&format!(
            "- mode: `{}` · ds binary: `{}` · created: {}\n\n",
            self.mode, self.ds_bin, self.created_at
        ));
        for sc in &self.scenarios {
            out.push_str(&format!(
                "## {} ({}, headroom {})\n\n",
                sc.scenario, sc.mode, sc.headroom
            ));
            for run in &sc.runs {
                let hr = match run.headroom {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "on(default)",
                };
                out.push_str(&format!(
                    "### run {} (headroom {hr}) — {} — ${:.6}\n\n",
                    run.run_index + 1,
                    if run.passed() { "PASS" } else { "FAIL" },
                    run.cost_usd
                ));
                out.push_str("| turn | input | cache_read | output | reasoning | cache-hit % |\n|---|---|---|---|---|---|\n");
                for (i, row) in run.rows.iter().enumerate() {
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {} | {:.1}% |\n",
                        i + 1,
                        row.input_tokens,
                        row.cache_read_tokens,
                        row.output_tokens,
                        row.reasoning_tokens,
                        row.cache_hit_ratio() * 100.0
                    ));
                }
                out.push_str(&format!(
                    "\n**totals:** input {} · cache_read {} · output {} · reasoning {} · cache-hit {:.1}% · **cost ${:.6}**\n\n",
                    run.totals.input,
                    run.totals.cache_read,
                    run.totals.output,
                    run.totals.reasoning,
                    run.cache_hit_pct,
                    run.cost_usd
                ));
                if let Some(pct) = run.compression_reduction_pct {
                    out.push_str(&format!(
                        "- headroom compression reduction on the large result: **{:.1}%**\n",
                        pct * 100.0
                    ));
                }
                if run.compaction_fired {
                    out.push_str("- auto-compaction fired (`ds-compact-*` request on the wire)\n");
                }
                for note in &run.failure_notes {
                    out.push_str(&format!("- ⚠ {note}\n"));
                }
                for a in &run.wire_assertions {
                    out.push_str(&format!("- ❌ wire: {a}\n"));
                }
                for a in &run.gate_assertions {
                    out.push_str(&format!("- ❌ gate: {a}\n"));
                }
                out.push('\n');
            }
        }
        if let Some(sav) = &self.savings {
            out.push_str("## Headroom A/B savings\n\n");
            match (sav.on_cost_usd, sav.off_cost_usd) {
                (Some(on), Some(off)) => {
                    out.push_str(&format!(
                        "- ON: ${on:.6} · OFF: ${off:.6} · delta: ${:.6} ({:.1}%)\n",
                        sav.delta_usd.unwrap_or(0.0),
                        if off > 0.0 {
                            (off - on) / off * 100.0
                        } else {
                            0.0
                        }
                    ));
                    out.push_str(&format!(
                        "- ON ≤ OFF: {} (measured cost, both recorded)\n",
                        sav.saves_money()
                    ));
                }
                _ => out.push_str("- A/B not available (missing ON or OFF run)\n"),
            }
            if let Some(pct) = sav.compression_reduction_pct {
                out.push_str(&format!(
                    "- shipped-function compression reduction on the large result: {:.1}%\n",
                    pct * 100.0
                ));
            }
        }
        out
    }
}

/// Build a run report from captured rows + assertion results.
#[allow(clippy::too_many_arguments)]
pub fn build_run_report(
    run_index: usize,
    headroom: Option<bool>,
    rows: &[UsageRow],
    markers_ok: bool,
    wire_assertions: Vec<String>,
    gate_assertions: Vec<String>,
    compaction_fired: bool,
    compression_reduction_pct: Option<f64>,
    failure_notes: Vec<String>,
) -> RunReport {
    let totals = TokenTotals::from_rows(rows);
    let cost = rows_cost_usd(rows);
    RunReport {
        run_index,
        headroom,
        rows: rows.to_vec(),
        cache_hit_pct: totals.cache_hit_ratio() * 100.0,
        totals,
        cost_usd: cost,
        markers_ok,
        wire_assertions,
        gate_assertions,
        compaction_fired,
        compression_reduction_pct,
        failure_notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u64, input: u64, cache: u64, output: u64) -> UsageRow {
        UsageRow {
            seq,
            conv_id: "c".into(),
            req_id: format!("r{seq}"),
            model: "deepseek-v4-flash".into(),
            input_tokens: input,
            cache_read_tokens: cache,
            output_tokens: output,
            reasoning_tokens: 0,
            total_tokens: input + output,
        }
    }

    #[test]
    fn savings_detects_on_le_off() {
        let s = SavingsReport {
            on_cost_usd: Some(0.0001),
            off_cost_usd: Some(0.0003),
            delta_usd: Some(0.0002),
            on_pass: true,
            off_pass: true,
            compression_reduction_pct: Some(0.94),
        };
        assert!(s.saves_money());
        let s2 = SavingsReport {
            on_cost_usd: Some(0.0004),
            off_cost_usd: Some(0.0003),
            ..s.clone()
        };
        assert!(!s2.saves_money());
    }

    #[test]
    fn report_marks_pass_and_fail() {
        let rows = vec![row(1, 1000, 900, 10), row(2, 2000, 0, 20)];
        let ok = build_run_report(
            1,
            Some(true),
            &rows,
            true,
            vec![],
            vec![],
            false,
            None,
            vec![],
        );
        assert!(ok.passed());
        let bad = build_run_report(
            2,
            Some(false),
            &rows,
            false,
            vec!["wire: nope".into()],
            vec![],
            false,
            None,
            vec![],
        );
        assert!(!bad.passed());
        // Cost identity: rows × rates == report cost (independent recompute).
        let totals = TokenTotals::from_rows(&rows);
        let rates = ds_models::rates_for_model("deepseek-v4-flash").unwrap();
        let expected = ds_models::estimate_cost_usd_from_full_input(
            totals.input,
            totals.cache_read,
            totals.output,
            &rates,
        );
        assert!((ok.cost_usd - expected).abs() < 1e-12);
    }
}
