//! Orchestration: scenarios × headroom modes → real `ds` sessions → captures →
//! assertions → report. Deterministic by construction in mock mode (scripted
//! model behavior, hermetic HOME, body-derived usage); live mode forwards to
//! the real gateway and parses its usage object.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::assertions::{
    check_completion_gate, check_compression_reduction, check_conversation_to_chat_messages_reasoning_rule,
    check_headroom_wire_and_roundtrip, check_parallel_attackers, check_wire_reasoning_rules,
};
use crate::mock::{RecordingServer, ServerConfig};
use crate::report::{
    HarnessReport, RunReport, SavingsReport, ScenarioReport, build_run_report,
};
use crate::scenarios::{Scenario, substitute_scenario};
use crate::session::{HomeConfig, SessionHome, run_ds};
use crate::usage::UsageRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mock,
    Live,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mode: Mode,
    pub ds_bin: PathBuf,
    pub out_dir: PathBuf,
    pub scenarios: Vec<Scenario>,
    /// Big-tool A/B order (ON, OFF, ON per the plan); other scenarios run ON.
    pub ab_order: Vec<Option<bool>>,
    pub live_api_key: Option<String>,
}

const LIVE_UPSTREAM: &str = "https://api.deepseek.com";
const LIVE_SETTINGS_URL: &str = "https://api.deepseek.com/v1";
const MOCK_KEY: &str = "sk-harness-mock-key";

pub async fn run_all(opts: &RunOptions) -> Result<HarnessReport> {
    std::fs::create_dir_all(&opts.out_dir).context("create out dir")?;
    let mut report = HarnessReport {
        mode: match opts.mode {
            Mode::Mock => "mock".into(),
            Mode::Live => "live".into(),
        },
        ds_bin: opts.ds_bin.display().to_string(),
        created_at: chrono_now(),
        ..Default::default()
    };

    for sc in &opts.scenarios {
        let modes: Vec<Option<bool>> = if sc.id == "big_tool" {
            opts.ab_order.clone()
        } else {
            vec![opts.ab_order.first().copied().unwrap_or(Some(true))]
        };
        let mut scenario_report = ScenarioReport {
            scenario: sc.id.to_string(),
            mode: report.mode.clone(),
            headroom: String::new(),
            ..Default::default()
        };
        for (i, headroom) in modes.iter().enumerate() {
            let run = run_scenario(opts, sc, *headroom, i).await?;
            scenario_report.headroom = if *headroom == Some(false) { "off" } else { "on" }.into();
            scenario_report.runs.push(run);
        }
        scenario_report.pass = scenario_report.runs.iter().all(|r| r.passed());
        report.scenarios.push(scenario_report);
    }

    report.savings = build_savings(&report);
    Ok(report)
}

async fn run_scenario(
    opts: &RunOptions,
    sc: &Scenario,
    headroom: Option<bool>,
    run_index: usize,
) -> Result<RunReport> {
    let hr_label = if headroom == Some(false) { "off" } else { "on" };
    let run_dir = opts
        .out_dir
        .join(sc.id)
        .join(format!("headroom_{hr_label}"))
        .join(format!("run{}", run_index + 1));
    std::fs::create_dir_all(&run_dir).context("create run dir")?;

    // Scenario cwd with fixture files.
    let cwd = tempfile::tempdir().context("scenario cwd")?;
    for (name, content) in &sc.fixtures {
        std::fs::write(cwd.path().join(name), content).context("write fixture")?;
    }

    // Expected formatted content of the big fixture (read_file wire shape) and
    // its shipped-function headroom hash — used for the scripted retrieve
    // turn and the byte-exact round-trip assertion.
    let formatted_fixture_a = sc
        .fixtures
        .iter()
        .find(|(n, _)| n == "fixture_a.txt")
        .map(|(_, c)| crate::scenarios::read_file_formatted(c));
    let retrieve_hash = formatted_fixture_a
        .as_deref()
        .and_then(|f| crate::assertions::headroom_compress(f).1)
        .unwrap_or_default();

    let sc = substitute_scenario(sc.clone(), cwd.path(), &retrieve_hash);

    // Capture server.
    let session_id = uuid::Uuid::new_v4().to_string();
    crate::mock::reset_script_consumption();
    let mut server = RecordingServer::start(ServerConfig {
        script: sc.script.clone(),
        main_conv_id: session_id.clone(),
        out_dir: run_dir.clone(),
        forward: match opts.mode {
            Mode::Mock => None,
            Mode::Live => Some(LIVE_UPSTREAM.to_string()),
        },
    })
    .await
    .context("start recording server")?;
    let port = server.addr.port();

    // Session home (isolated, hermetic in mock; real key in live).
    let api_key = match opts.mode {
        Mode::Mock => MOCK_KEY.to_string(),
        Mode::Live => opts
            .live_api_key
            .clone()
            .context("live mode requires an API key (set DEEPSEEK_API_KEY)")?,
    };
    let home = SessionHome::setup(
        &opts.ds_bin,
        &HomeConfig {
            api_key,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            ds_api_base_url: match opts.mode {
                Mode::Mock => format!("http://127.0.0.1:{port}/v1"),
                Mode::Live => LIVE_SETTINGS_URL.to_string(),
            },
            model: "deepseek-v4-flash".into(),
            context_window: sc.context_window,
            subagents: sc.subagents,
            headroom,
        },
    )
    .context("session home setup")?;

    // Run the user turns (chained on one session).
    let mut invocation_outputs = Vec::new();
    let mut failure_notes = Vec::new();
    for (i, prompt) in sc.user_turns.iter().enumerate() {
        let debug_log = run_dir.join(format!("turn{}.log", i + 1));
        let res = run_ds(
            &opts.ds_bin,
            &home,
            cwd.path(),
            prompt,
            &session_id,
            i == 0,
            sc.max_turns_per_invocation,
            &debug_log,
            headroom,
            sc.subagents,
            sc.web_search,
        );
        match res {
            Ok(r) => {
                std::fs::write(run_dir.join(format!("out{}.json", i + 1)), &r.stdout)
                    .context("write invocation output")?;
                if r.output.failed() {
                    failure_notes.push(format!(
                        "turn {} failed: {}",
                        i + 1,
                        r.output.message.clone().unwrap_or_default()
                    ));
                }
                invocation_outputs.push(r);
            }
            Err(e) => {
                failure_notes.push(format!("turn {} spawn error: {e:#}", i + 1));
            }
        }
    }

    // Collect captures + assertions.
    let (wire, rows) = server.stop().await.context("stop recording server")?;

    let markers_ok = check_markers(&sc, &invocation_outputs, &mut failure_notes);
    let mut wire_assertions = check_wire_reasoning_rules(&wire);
    // Headroom wire-marker contract applies only to scenarios whose big read
    // reaches a request built by the request-builder path. The compaction
    // scenario's summary replaces the tool result before it is re-sent, and
    // parallel_attackers never reads the fixture — both would make the marker
    // check vacuous. Their contracts are asserted separately.
    let scripts_read_file = sc.script.iter().any(|i| {
        matches!(
            i,
            crate::scenarios::MockItem::Tool { name, .. } if name == "read_file"
        )
    });
    if scripts_read_file {
        if let Some(formatted) = &formatted_fixture_a {
            wire_assertions.extend(check_headroom_wire_and_roundtrip(
                &wire,
                &session_id,
                formatted,
                headroom != Some(false),
            ));
        }
    }
    if sc.id == "round_trip" {
        wire_assertions.extend(check_round_trip_specifics(&wire, &session_id, &run_dir));
    }
    if sc.id == "parallel_attackers" {
        let n = sc
            .script
            .iter()
            .filter(|i| {
                matches!(i, crate::scenarios::MockItem::Tool { name, .. } if name == "spawn_subagent")
            })
            .count();
        wire_assertions.extend(check_parallel_attackers(&wire, &session_id, n));
    }
    if sc.assert_compaction {
        let fired = rows.iter().any(|r| r.is_compaction());
        if !fired {
            wire_assertions.push("compaction scenario: no ds-compact-* request on the wire".into());
        }
    }
    let mut gate_assertions = check_completion_gate();
    gate_assertions.extend(check_conversation_to_chat_messages_reasoning_rule());

    let compression_reduction_pct = formatted_fixture_a
        .as_deref()
        .and_then(|f| check_compression_reduction(f).ok());

    let run = build_run_report(
        run_index,
        headroom,
        &rows,
        markers_ok,
        wire_assertions,
        gate_assertions,
        sc.assert_compaction && rows.iter().any(|r| r.is_compaction()),
        compression_reduction_pct,
        failure_notes,
    );

    // Artifact summary line for the terminal.
    eprintln!(
        "[cost-harness] {} headroom={} run {}: {} requests, {:.6} USD, {}",
        sc.id,
        hr_label,
        run_index + 1,
        rows.len(),
        run.cost_usd,
        if run.passed() { "PASS" } else { "FAIL" }
    );
    Ok(run)
}

/// Marker assertions per invocation (each user turn's expected output text).
fn check_markers(
    sc: &Scenario,
    outputs: &[crate::session::InvocationResult],
    failure_notes: &mut Vec<String>,
) -> bool {
    let mut ok = true;
    for (i, marker) in sc.expected_markers.iter().enumerate() {
        let is_final = i == sc.expected_markers.len() - 1;
        let inv_idx = if is_final {
            outputs.len().saturating_sub(1)
        } else {
            i
        };
        let Some(inv) = outputs.get(inv_idx) else {
            failure_notes.push(format!(
                "marker {marker}: no invocation output for turn {}",
                inv_idx + 1
            ));
            ok = false;
            continue;
        };
        let text = inv.output.text.clone().unwrap_or_default();
        if !text.contains(marker) {
            failure_notes.push(format!(
                "marker {marker} missing from invocation {} output (text: {:?})",
                inv_idx + 1,
                truncate(&text, 80)
            ));
            ok = false;
        }
    }
    ok
}

fn check_round_trip_specifics(
    wire: &[crate::mock::WireRequest],
    session_id: &str,
    run_dir: &Path,
) -> Vec<String> {
    let mut failures = Vec::new();
    let names = crate::assertions::tool_names_on_last_request(wire, session_id);
    for required in ["headroom_retrieve", "web_search", "spawn_subagent"] {
        if !names.iter().any(|n| n == required) {
            failures.push(format!("round_trip: {required} never called on the wire"));
        }
    }
    // The in-session headroom_retrieve result must carry the exact original
    // first line (HEADROOM_ORIGINAL marker + target line).
    let target_line = "TARGETWORD_R line 1";
    let mut saw_retrieve_result = false;
    for (_, content) in crate::assertions::tool_results_for_conv(wire, session_id) {
        if content.starts_with("HEADROOM_ORIGINAL") {
            saw_retrieve_result = true;
            if !content.contains(target_line) {
                failures.push("round_trip: headroom_retrieve result missing the exact original line".into());
            }
        }
    }
    if !saw_retrieve_result {
        failures.push("round_trip: no HEADROOM_ORIGINAL retrieve result on the wire".into());
    }
    // web_search DuckDuckGo fallback must have fired (debug log evidence).
    let mut saw_fallback = false;
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "log").unwrap_or(false) {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    if text.contains("web_search backend failed; falling back to DuckDuckGo") {
                        saw_fallback = true;
                    }
                    if text.contains("COMPLETION GATE") {
                        failures.push("round_trip: completion gate fired on a sub-step claim".into());
                    }
                }
            }
        }
    }
    if !saw_fallback {
        failures.push("round_trip: web_search DuckDuckGo fallback line not found in debug logs".into());
    }
    failures
}

fn build_savings(report: &HarnessReport) -> Option<SavingsReport> {
    let big = report.scenarios.iter().find(|s| s.scenario == "big_tool")?;
    let on_first = big.runs.iter().find(|r| r.headroom != Some(false));
    let off = big.runs.iter().find(|r| r.headroom == Some(false));
    let (Some(on_first), Some(off)) = (on_first, off) else {
        return Some(SavingsReport {
            on_pass: big.runs.iter().all(|r| r.passed()),
            off_pass: big.runs.iter().any(|r| r.headroom == Some(false)),
            ..Default::default()
        });
    };
    let on_cost = on_first.cost_usd;
    let off_cost = off.cost_usd;
    Some(SavingsReport {
        on_cost_usd: Some(on_cost),
        off_cost_usd: Some(off_cost),
        delta_usd: Some(off_cost - on_cost),
        on_pass: on_first.passed(),
        off_pass: off.passed(),
        compression_reduction_pct: on_first.compression_reduction_pct,
    })
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn chrono_now() -> String {
    // Avoid a chrono dep for one timestamp: format from SystemTime.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix-{secs}")
}

/// Recompute one run's cost from its raw rows (verification-plan identity).
pub fn recompute_run_cost(rows: &[UsageRow]) -> f64 {
    crate::cost::rows_cost_usd(rows)
}
