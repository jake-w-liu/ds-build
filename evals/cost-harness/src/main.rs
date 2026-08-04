//! ds-cost-harness CLI.
//!
//! ```text
//! cost-harness run [--mode mock|live] [--scenario all|<id>]
//!                 [--headroom on|off|both] [--out DIR] [--ds-bin PATH]
//! cost-harness scenarios
//! cost-harness expected-cost
//! ```

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use ds_cost_harness::runner::{Mode, RunOptions, run_all};
use ds_cost_harness::scenarios::all_scenarios;
use ds_cost_harness::session::{resolve_ds_bin, resolve_live_api_key};

#[derive(Parser)]
#[command(name = "cost-harness", version, about = "Cost-aware stress/verification harness for the DS Build DeepSeek pipeline")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the stress scenarios and write the report.
    Run {
        /// mock (free, deterministic) | live (real gateway, needs API key)
        #[arg(long, default_value = "mock")]
        mode: String,
        /// all | multi_turn | compaction | round_trip | big_tool
        #[arg(long, default_value = "all")]
        scenario: String,
        /// headroom policy: on | off | both (both = every scenario twice;
        /// big_tool always runs ON→OFF→ON for the savings A/B)
        #[arg(long, default_value = "both")]
        headroom: String,
        /// output directory for report + captures
        #[arg(long, default_value = "./cost-harness-out")]
        out: PathBuf,
        /// parallel_attackers batch size (default 4; raise for 20+ tests)
        #[arg(long, default_value_t = 4)]
        attackers: usize,
        /// ds binary path (default: `ds` on PATH)
        #[arg(long)]
        ds_bin: Option<PathBuf>,
    },
    /// List the stress scenarios.
    Scenarios,
    /// Print pinned rates + the documented expected live-pass cost.
    ExpectedCost,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            mode,
            scenario,
            headroom,
            out,
            attackers,
            ds_bin,
        } => {
            let mode = match mode.as_str() {
                "mock" => Mode::Mock,
                "live" => Mode::Live,
                other => anyhow::bail!("unknown --mode {other:?} (mock|live)"),
            };
            let ds_bin = match ds_bin {
                Some(p) => p,
                None => resolve_ds_bin()?,
            };
            let live_api_key = match mode {
                Mode::Mock => None,
                Mode::Live => {
                    let key = resolve_live_api_key()?;
                    eprintln!(
                        "[cost-harness] live mode: API key resolved from env/config (not printed)"
                    );
                    Some(key)
                }
            };

            let mut scenarios = all_scenarios();
            if scenario != "all" {
                scenarios.retain(|s| s.id == scenario);
                anyhow::ensure!(
                    !scenarios.is_empty(),
                    "unknown scenario {scenario:?} (all|multi_turn|compaction|round_trip|big_tool|parallel_attackers)"
                );
            }
            if attackers != 4 {
                // Rebuild parallel_attackers at the requested batch size.
                anyhow::ensure!(attackers >= 1 && attackers <= 256, "--attackers must be 1..=256");
                let pa = ds_cost_harness::scenarios::parallel_attackers("TARGETWORD_P", attackers);
                scenarios.retain(|s| s.id != "parallel_attackers");
                scenarios.push(pa);
            }

            let ab_order = match headroom.as_str() {
                "on" => vec![Some(true)],
                "off" => vec![Some(false)],
                "both" => vec![Some(true), Some(false), Some(true)],
                other => anyhow::bail!("unknown --headroom {other:?} (on|off|both)"),
            };

            if mode == Mode::Live {
                print_expected_live_cost();
            }

            let report = run_all(&RunOptions {
                mode,
                ds_bin,
                out_dir: out.clone(),
                scenarios,
                ab_order,
                live_api_key,
            })
            .await?;

            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join("report.json"), report.render_json())?;
            std::fs::write(out.join("report.md"), report.render_markdown())?;

            println!("{}", report.render_markdown());
            let ok = report.all_scenarios_pass()
                && report
                    .savings
                    .as_ref()
                    .map(|s| s.on_pass && s.off_pass && s.saves_money())
                    .unwrap_or(true);
            if !ok {
                eprintln!("[cost-harness] gating assertions FAILED — see report.json");
                std::process::exit(1);
            }
            eprintln!(
                "[cost-harness] report written to {} (report.json / report.md)",
                out.display()
            );
            Ok(())
        }
        Command::Scenarios => {
            for sc in all_scenarios() {
                println!(
                    "{:<12} window={:<8} turns={:<2} script_items={:<2} fixtures={}",
                    sc.id,
                    sc.context_window,
                    sc.user_turns.len(),
                    sc.script.len(),
                    sc.fixtures
                        .iter()
                        .map(|(n, c)| format!("{n}({} chars)", c.len()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(())
        }
        Command::ExpectedCost => {
            print_expected_live_cost();
            Ok(())
        }
    }
}

/// Documented expected live cost before the first live run (flash list
/// rates, pinned in ds-models).
fn print_expected_live_cost() {
    let r = ds_models::rates_for_model("deepseek-v4-flash").unwrap();
    eprintln!(
        "[cost-harness] LIVE MODE expected cost (deepseek-v4-flash, pinned list rates):"
    );
    eprintln!(
        "[cost-harness]   cache_hit ${}/M · cache_miss ${}/M · output ${}/M (reasoning billed as output)",
        r.cache_hit_per_mtok, r.cache_miss_per_mtok, r.output_per_mtok
    );
    eprintln!(
        "[cost-harness]   full pass ≈ 180–250k prompt tokens (mostly cache-hit after warmup) \
         + ~15k output → roughly $0.02–0.05"
    );
}
