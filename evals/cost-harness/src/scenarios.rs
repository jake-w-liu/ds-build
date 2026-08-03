//! Stress-scenario definitions (config, not I/O).
//!
//! Every scenario runs through the REAL `ds` binary (headless
//! `-p --output-format json`); the mock's scripted model behavior keeps the
//! session deterministic while all tool execution (read_file, bash, grep,
//! web_search fallback, spawn_subagent, headroom_retrieve) is real and local.

use serde::{Deserialize, Serialize};
use serde_json::json;

/// One scripted model response the mock serves to the main conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MockItem {
    /// Emit a `tool_calls` stream (with reasoning_content delta) for `name`
    /// with `args`. The agent executes the tool for real.
    Tool { name: String, args: serde_json::Value },
    /// Emit MULTIPLE tool calls in ONE response — a true parallel batch.
    Batch(Vec<(String, serde_json::Value)>),
    /// Emit a plain text stream (no reasoning).
    Text { text: String },
}

impl MockItem {
    pub fn tool(name: &str, args: serde_json::Value) -> Self {
        Self::Tool {
            name: name.to_string(),
            args,
        }
    }
    pub fn text(text: &str) -> Self {
        Self::Text {
            text: text.to_string(),
        }
    }
}

/// A complete stress scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: &'static str,
    /// context_window written into the session config (small → compaction).
    pub context_window: u64,
    /// Scripted model behavior for the main conversation, in request order.
    pub script: Vec<MockItem>,
    /// User prompts, chained onto one session (`--session-id` then `--resume`).
    pub user_turns: Vec<String>,
    pub max_turns_per_invocation: u32,
    /// Strings that must appear in the session's final output text.
    pub expected_markers: Vec<String>,
    /// (file_name, content) fixture files written into the scenario cwd.
    pub fixtures: Vec<(String, String)>,
    /// Assert an auto-compaction request (`ds-compact-*`) fired on the wire.
    pub assert_compaction: bool,
    /// Minimum recorded usage rows (sanity floor, derived from script size).
    pub min_usage_rows: usize,
    /// Subagent tool available to the agent (needed for spawn_subagent).
    pub subagents: bool,
    /// web_search tool available (round-trip scenario only).
    pub web_search: bool,
}

/// Deterministic fixture text: `N` lines, each `~40` chars, containing the
/// unique `target` word on line 1 so grep finds exactly one match.
/// No trailing newline so `read_file`'s `N→` line-prefix formatting round-trips.
pub fn fixture_content(lines: usize, target: &str) -> String {
    let mut out = String::with_capacity(lines * 42);
    for i in 1..=lines {
        if i == 1 {
            out.push_str(&format!("{target} line {i} of the deterministic stress fixture\n"));
        } else {
            out.push_str(&format!(
                "LINE{i:04} filler content aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee\n"
            ));
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The `read_file` tool's output formatting for `content` (real tool rule,
/// ds-tools/src/implementations/ds_build/read_file/mod.rs): the first visible
/// line and every 10th line are prefixed `N→`, other lines are bare; lines
/// are joined with `\n` and no trailing newline is added. The harness
/// replicates this exactly so the headroom hash computed here equals the hash
/// the client stores on the wire.
pub fn read_file_formatted(content: &str) -> String {
    let mut out = String::new();
    for (i, line) in content.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let n = i + 1;
        if n == 1 || n % 10 == 0 {
            out.push_str(&format!("{n}→{line}"));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Fixture content size in chars for `fixture_content(lines, _)`.
pub fn fixture_chars(lines: usize) -> usize {
    fixture_content(lines, "TARGET").len()
}

pub const BIG_FIXTURE_LINES: usize = 800; // ~33k chars → ~8k tokens formatted
pub const SMALL_FIXTURE_LINES: usize = 300;

fn big_fixture(target: &str) -> String {
    fixture_content(BIG_FIXTURE_LINES, target)
}

/// Scenario (a): long multi-turn session (5 user turns) with large tool
/// results exercising headroom compression across turns.
pub fn multi_turn(target: &str) -> Scenario {
    let fa = big_fixture(target);
    let fb = fixture_content(SMALL_FIXTURE_LINES, "SECOND");
    Scenario {
        id: "multi_turn",
        context_window: 1_000_000,
        script: vec![
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_A}" })),
            MockItem::text("STEP1_OK"),
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_B}" })),
            MockItem::text("STEP2_OK"),
            MockItem::tool(
                "grep",
                json!({ "pattern": target, "path": "{FIXTURE_A}" }),
            ),
            MockItem::text("STEP3_OK"),
            MockItem::tool(
                "run_terminal_command",
                json!({ "command": "ls {CWD}", "description": "list scenario cwd" }),
            ),
            MockItem::text("STEP4_OK"),
            MockItem::text("MULTI_TURN_DONE"),
        ],
        user_turns: vec![
            format!("Read the file {} fully and reply: STEP1_OK", fa_path()),
            format!("Now read {} and reply: STEP2_OK", fb_path()),
            format!(
                "Grep for the word {target} in {} and reply: STEP3_OK",
                fa_path()
            ),
            format!("Run the command: ls {} and reply: STEP4_OK", cwd_path()),
            "Reply with exactly: MULTI_TURN_DONE".to_string(),
        ],
        max_turns_per_invocation: 6,
        expected_markers: vec![
            "STEP1_OK".into(),
            "STEP2_OK".into(),
            "STEP3_OK".into(),
            "STEP4_OK".into(),
            "MULTI_TURN_DONE".into(),
        ],
        fixtures: vec![
            ("fixture_a.txt".into(), fa),
            ("fixture_b.txt".into(), fb),
        ],
        assert_compaction: false,
        min_usage_rows: 9,
        subagents: false,
        web_search: false,
    }
}

/// Scenario (b): compaction-crossing. Two large reads push the stored
/// conversation estimate over the 85% auto-compact line (window 20k) on the
/// second turn, so the client emits a `ds-compact-*` request exactly once and
/// the post-compact continuation (~9k with system prompt + summary) fits
/// under the new threshold — no compaction loop (a loop is what a window of
/// 6k produced live: continuation ≈ 9.5k > 85% of 6k).
pub fn compaction(target: &str) -> Scenario {
    let fc = big_fixture(target);
    Scenario {
        id: "compaction",
        context_window: 20_000,
        script: vec![
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_C}" })),
            MockItem::text("STEPC1_OK"),
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_A}" })),
            MockItem::text("COMPACTION_DONE"),
        ],
        user_turns: vec![
            format!("Read the file {} fully and reply: STEPC1_OK", fc_path()),
            format!(
                "Now read {} fully and reply: COMPACTION_DONE",
                fa_path()
            ),
        ],
        max_turns_per_invocation: 8,
        expected_markers: vec!["STEPC1_OK".into(), "COMPACTION_DONE".into()],
        fixtures: vec![
            ("fixture_c.txt".into(), fc),
            ("fixture_a.txt".into(), big_fixture(target)),
        ],
        assert_compaction: true,
        min_usage_rows: 6,
        subagents: false,
        web_search: false,
    }
}

/// Scenario (c): tool round-trip — read_file (large, headroom-compressed),
/// in-session `headroom_retrieve` (byte-exact original), bash, web_search
/// (backend 400 → DuckDuckGo fallback), subagent spawn whose reply is a
/// non-gated sub-step claim ("Build finished.").
pub fn round_trip(target: &str, retrieve_hash: &str) -> Scenario {
    let fa = big_fixture(target);
    Scenario {
        id: "round_trip",
        context_window: 1_000_000,
        script: vec![
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_A}" })),
            MockItem::tool(
                "headroom_retrieve",
                json!({ "hash": retrieve_hash, "query": target }),
            ),
            MockItem::tool(
                "run_terminal_command",
                json!({ "command": "ls {CWD}", "description": "list scenario cwd" }),
            ),
            MockItem::tool(
                "web_search",
                json!({ "query": "deepseek api pricing cache hit miss" }),
            ),
            MockItem::tool(
                "spawn_subagent",
                json!({
                    "prompt": "Reply with exactly: Build finished.",
                    "description": "completion-gate negative check",
                    "subagent_type": "general-purpose"
                }),
            ),
            MockItem::text("ROUND_TRIP_DONE"),
        ],
        user_turns: vec![format!(
            "Work through these steps with your tools: read {} fully, retrieve its original first line via headroom_retrieve, list {}, run a web search for deepseek pricing, and spawn a subagent that replies 'Build finished.'. Then reply: ROUND_TRIP_DONE",
            fa_path(),
            cwd_path()
        )],
        max_turns_per_invocation: 12,
        expected_markers: vec!["ROUND_TRIP_DONE".into()],
        fixtures: vec![("fixture_a.txt".into(), fa)],
        assert_compaction: false,
        min_usage_rows: 7,
        subagents: true,
        web_search: true,
    }
}

/// Scenario (d): the headroom A/B workhorse — one large read, minimal script.
pub fn big_tool(target: &str) -> Scenario {
    let fa = big_fixture(target);
    Scenario {
        id: "big_tool",
        context_window: 1_000_000,
        script: vec![
            MockItem::tool("read_file", json!({ "target_file": "{FIXTURE_A}" })),
            MockItem::text("BIG_TOOL_DONE"),
        ],
        user_turns: vec![format!(
            "Read the file {} fully and reply: BIG_TOOL_DONE",
            fa_path()
        )],
        max_turns_per_invocation: 6,
        expected_markers: vec!["BIG_TOOL_DONE".into()],
        fixtures: vec![("fixture_a.txt".into(), fa)],
        assert_compaction: false,
        min_usage_rows: 3,
        subagents: false,
        web_search: false,
    }
}

/// Scenario (e): parallel adversarial review — the upgrade's core proof.
/// One model turn spawns N attacker-math critics in a SINGLE parallel batch
/// (run_in_background=true, one scoped assignment each), then one turn
/// collects ALL outputs before the final text. Asserts: all N spawns in one
/// request, attacker-* background respected (no limit rejection), all N ids
/// harvested into the fetch call, session completes.
pub fn parallel_attackers(target: &str, n: usize) -> Scenario {
    let fa = big_fixture(target);
    let spawns: Vec<(String, serde_json::Value)> = (0..n)
        .map(|i| {
            (
                "spawn_subagent".to_string(),
                json!({
                    "prompt": format!(
                        "Scoped adversarial review #{i}: independently recompute the final result \
                         for fixture_a.txt line {} (the {target} line) — residual, units, regimes. \
                         Report verdict + evidence only.",
                        i + 1
                    ),
                    "description": format!("parallel attacker #{i}"),
                    "subagent_type": "attacker-math",
                    "run_in_background": true
                }),
            )
        })
        .collect();
    let mut script = vec![MockItem::Batch(spawns)];
    script.push(MockItem::tool(
        "get_command_or_subagent_output",
        json!({
            "task_ids": ["__SUBAGENT_IDS__"],
            "timeout_ms": 300_000
        }),
    ));
    script.push(MockItem::text("PARALLEL_ATTACKERS_DONE"));
    Scenario {
        id: "parallel_attackers",
        context_window: 1_000_000,
        script,
        user_turns: vec![format!(
            "Spawn {n} attacker-math critics IN PARALLEL (background), one per scoped check on \
             the file {}, then collect every output before replying: PARALLEL_ATTACKERS_DONE",
            fa_path()
        )],
        max_turns_per_invocation: 12,
        expected_markers: vec!["PARALLEL_ATTACKERS_DONE".into()],
        fixtures: vec![("fixture_a.txt".into(), fa)],
        assert_compaction: false,
        min_usage_rows: 8,
        subagents: true,
        web_search: false,
    }
}

pub fn all_scenarios() -> Vec<Scenario> {
    // The retrieve hash for round_trip is computed at run time from the real
    // fixture content via the shipped ds_headroom functions; the placeholder
    // is substituted by the runner (see runner.rs). The hash value here is a
    // stable stand-in so `scenarios` listing and config tests stay pure.
    vec![
        multi_turn("TARGETWORD_A"),
        compaction("TARGETWORD_C"),
        round_trip("TARGETWORD_R", "<hash-computed-at-runtime>"),
        big_tool("TARGETWORD_B"),
        parallel_attackers("TARGETWORD_P", 4),
    ]
}

// Placeholder paths are substituted by the runner when the scenario cwd is
// known. These helpers mirror the substitution keys for clarity.
pub fn fa_path() -> &'static str {
    "{FIXTURE_A}"
}
pub fn fb_path() -> &'static str {
    "{FIXTURE_B}"
}
pub fn fc_path() -> &'static str {
    "{FIXTURE_C}"
}
pub fn cwd_path() -> &'static str {
    "{CWD}"
}

/// Substitute `{FIXTURE_A|B|C}` / `{CWD}` placeholders in a scenario's
/// prompts/script args with concrete absolute paths.
pub fn substitute_scenario(mut sc: Scenario, cwd: &std::path::Path, hash: &str) -> Scenario {
    let fa = cwd.join("fixture_a.txt").to_string_lossy().to_string();
    let fb = cwd.join("fixture_b.txt").to_string_lossy().to_string();
    let fc = cwd.join("fixture_c.txt").to_string_lossy().to_string();
    let cwd_s = cwd.to_string_lossy().to_string();
    let subst = |s: &str| {
        s.replace("{FIXTURE_A}", &fa)
            .replace("{FIXTURE_B}", &fb)
            .replace("{FIXTURE_C}", &fc)
            .replace("{CWD}", &cwd_s)
    };
    sc.user_turns = sc.user_turns.iter().map(|t| subst(t)).collect();
    for item in &mut sc.script {
        match item {
            MockItem::Tool { args, .. } => {
                let s = serde_json::to_string(args).unwrap();
                let s = subst(&s).replace("<hash-computed-at-runtime>", hash);
                *args = serde_json::from_str(&s).expect("substitution keeps valid JSON");
            }
            MockItem::Batch(items) => {
                for (_, args) in items {
                    let s = serde_json::to_string(args).unwrap();
                    let s = subst(&s).replace("<hash-computed-at-runtime>", hash);
                    *args = serde_json::from_str(&s).expect("substitution keeps valid JSON");
                }
            }
            MockItem::Text { text } => *text = subst(text),
        }
    }
    sc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_deterministic_and_sized() {
        let a = fixture_content(BIG_FIXTURE_LINES, "TARGETWORD_A");
        let b = fixture_content(BIG_FIXTURE_LINES, "TARGETWORD_A");
        assert_eq!(a, b, "fixture must be deterministic across calls");
        assert!(a.len() > 30_000, "big fixture should be ~33k chars, got {}", a.len());
        assert!(!a.ends_with('\n'));
        assert!(a.starts_with("TARGETWORD_A line 1"));
    }

    #[test]
    fn read_file_formatting_matches_wire_shape() {
        let content = "alpha\nbeta\ngamma\ndelta\ne";
        // First line and every 10th line get the N→ prefix (shipped rule).
        assert_eq!(read_file_formatted(content), "1→alpha\nbeta\ngamma\ndelta\ne");
        let ten_lines = (1..=10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let formatted = read_file_formatted(&ten_lines);
        assert!(formatted.starts_with("1→l1"));
        assert!(formatted.ends_with("10→l10"));
    }

    #[test]
    fn scenario_scripts_have_tool_then_text_shape() {
        for sc in all_scenarios() {
            assert!(
                matches!(
                    sc.script.first(),
                    Some(MockItem::Tool { .. }) | Some(MockItem::Batch(_))
                ),
                "{}: first script item must be a tool call or parallel batch",
                sc.id
            );
            assert!(
                matches!(sc.script.last(), Some(MockItem::Text { .. })),
                "{}: last script item must be final text",
                sc.id
            );
            let tool_turns = sc
                .script
                .iter()
                .filter(|i| matches!(i, MockItem::Tool { .. }))
                .count();
            assert!(tool_turns >= 1);
            assert!(
                sc.min_usage_rows >= sc.script.len(),
                "{}: usage floor must cover the script",
                sc.id
            );
        }
    }

    #[test]
    fn substitution_applies_paths_and_hash() {
        let mut sc = all_scenarios().remove(2); // round_trip with placeholder hash
        sc = substitute_scenario(sc, std::path::Path::new("/tmp/abc"), "hash123");
        assert!(sc.user_turns[0].contains("/tmp/abc/fixture_a.txt"));
        match &sc.script[1] {
            MockItem::Tool { name, args } => {
                assert_eq!(name, "headroom_retrieve");
                assert_eq!(args["hash"], "hash123");
            }
            other => panic!("unexpected item: {other:?}"),
        }
        assert_eq!(
            serde_json::to_string(&sc.script[1]).unwrap(),
            format!(
                "{{\"Tool\":{{\"name\":\"headroom_retrieve\",\"args\":{{\"hash\":\"hash123\",\"query\":\"TARGETWORD_R\"}}}}}}"
            )
        );
        assert!(
            !serde_json::to_string(&sc.script[1])
                .unwrap()
                .contains("<hash-computed-at-runtime>")
        );
    }
}
