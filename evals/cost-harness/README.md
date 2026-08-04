# ds-cost-harness

Cost-aware stress/verification harness for the DS Build DeepSeek pipeline.

**Engineering goal, measured not assumed:**
1. **Save tokens and money** — headroom ON vs OFF A/B on a big-tool-result
   scenario, with cost computed from real per-request usage rows × the pinned
   `ds-models` rates, and compression stats from the shipped `ds-headroom`
   functions (≥90% reduction required).
2. **Ensure correctness** — stress scenarios run through the REAL `ds` binary
   (headless `-p --output-format json`); wire-contract assertions
   (`reasoning_content` placement, headroom marker + byte-exact retrieve,
   completion gate) drive the shipped functions.

## Scenarios

| id | window | what it stresses |
|---|---|---|
| `multi_turn` | 1M | 5 chained user turns, large tool results compressed by headroom across turns, grep + bash round-trip |
| `compaction` | 20k | two large reads push the stored-conversation estimate past the 85% auto-compact line → `ds-compact-*` request on the wire → session continues and completes |
| `round_trip` | 1M | read_file (large) → in-session `headroom_retrieve` (byte-exact original) → bash → web_search (backend 400 → DuckDuckGo fallback) → subagent spawn ("Build finished." must NOT gate) |
| `big_tool` | 1M | one large read; runs ON → OFF → ON for the savings A/B |
| `parallel_attackers` | 1M | N attacker-math critics spawned in ONE parallel batch (background=true, scoped assignments), all outputs collected before the final text — proves the parallel adversarial-review upgrade |

## Modes

- **mock (default, free, deterministic, CI-safe):** a local recording server
  serves `/v1/chat/completions` with scripted tool-call/text streams and usage
  derived from the request body it actually received — so headroom ON (marker,
  small body) vs OFF (full content, big body) shows up as real cost
  differences. Every request is captured to `wire.jsonl` / `usage.jsonl`.
  The only network egress is the web_search DuckDuckGo fallback (public,
  keyless).
- **live:** a recording forward-proxy relays to `api.deepseek.com` and parses
  the real gateway usage object from the SSE stream — same capture format,
  same cost math. Requires `DEEPSEEK_API_KEY` (or `DS_API_KEY`, or the key in
  `~/.ds/config.toml`). The key is read at runtime, never committed.

## Usage

```bash
cargo build -p ds-cost-harness

# list scenarios
./target/debug/cost-harness scenarios

# full mock pass (gating): every scenario + the big_tool A/B
DS_BIN=$(which ds) ./target/debug/cost-harness run --mode mock --out out/mock-run

# single scenario
DS_BIN=$(which ds) ./target/debug/cost-harness run --mode mock --scenario big_tool --out out/ab

# live pass (real gateway, real cost)
DS_BIN=$(which ds) DEEPSEEK_API_KEY=sk-… ./target/debug/cost-harness run --mode live --out out/live

# headroom policy: on | off | both (big_tool always runs ON→OFF→ON)
./target/debug/cost-harness run --headroom on --scenario compaction --out out/x
```

Exit code 0 = all gating assertions passed; otherwise 1 with the failures in
`report.json` / `report.md`.

## Cost identity (how numbers are reproducible)

Every report number is recomputable:

```
per-request cost = estimate_cost_usd_from_full_input(
    input_tokens, cache_read_tokens, output_tokens, rates_for_model(model))
    # = uncached/1e6 * cache_miss_per_mtok + cached/1e6 * cache_hit_per_mtok
    #   + output/1e6 * output_per_mtok
report scenario cost = Σ per-request costs over usage.jsonl rows
```

- Rows come from `usage.jsonl` (mock: body-derived `chars/4`; live: the real
  gateway usage object parsed from the SSE stream).
- Rates are the repo-pinned `ds-models` table (deepseek-v4-flash:
  cache-hit $0.0028/M, cache-miss $0.14/M, output $0.28/M). Reasoning is
  billed as output (the gateway reports `output_tokens` including reasoning).
- `cargo test -p ds-cost-harness` locks this identity against a real captured
  usage fixture + hand-computed constants.

## Expected live cost (documented before first run)

Flash list rates: full pass ≈ 150–250k prompt tokens (mostly cache-hit after
warm-up) + ~15k output → **roughly $0.02–0.05**. The big_tool A/B is the only
repeated-live-cost measurement; run it first with `--scenario big_tool` if you
want the cheapest live confirmation of the savings claim.

## Determinism

Two consecutive identical mock launches must pass with the same assertions
(the acceptance gate runs the harness twice). Cache-warm-up effects are
reported (cache_read trajectory), never asserted equal.

## Findings from stress runs

- **Compaction loop at small windows (shipped client).** With `context_window =
  6000` the live run produced **16 compaction requests and an empty final
  output**: the post-compact continuation (~9.5k tokens — real system prompt +
  summary + query) exceeds any window that would trigger compaction on turn 1
  (~8.6k), so every continuation re-triggers compaction. The harness's
  compaction scenario therefore uses a 20k window with two reads (trigger on
  turn 2, continuation fits) — compaction fires once and the session
  completes. The loop is a real client behavior worth a follow-up look at the
  auto-compact trigger/suppression logic; reproduced evidence lives in the
  scenario report (`compaction/headroom_on/run1/usage.jsonl` shows the
  `ds-compact-*` request sequence).
- **Headroom A/B (measured, both modes):** mock $0.002474 (ON) vs $0.004590
  (OFF) = 46% cheaper; live $0.0005–0.0008 (ON) vs $0.0033–0.0036 (OFF) =
  ~84% cheaper, with real cache hits at 81–91%.
- **Parallel adversarial review (orchestration upgrade, 0.1.75+):** attacker-*
  critics previously hard-forced foreground and capped at 3 per turn (the
  model spawned one, waited, spawned the next — the observed "only 1
  subagent" behavior). Now: **no hard cap on subagent counts** (0.1.76+),
  an explicit `run_in_background: true` overrides the attacker foreground
  default, and the orchestration guidance teaches single-batch parallel
  decomposition (one attacker per result/regime/claim, collect ALL before
  gating). The `parallel_attackers` scenario proves the runtime path: 4
  attacker-math spawns in ONE model response, all collected.

## Key hygiene

The harness reads live keys from the environment (`DEEPSEEK_API_KEY` /
`DS_API_KEY`) or `~/.ds/config.toml` at runtime — never from the repo. Test
fixtures use an obviously fake `sk-test-…` key. As a precaution, rotate any
DeepSeek API key that may have appeared in local working files during
development (an earlier draft fixture carried a real key; it was scrubbed from
the repo before any push).

## What the harness does NOT do

- Changes ds production behavior to win — it measures the shipped client.
- Model tuning, non-DeepSeek vendors, pricing beyond the pinned table.
- Image/vision input (text-only pipeline by design).
- Latency/throughput benchmarking.
