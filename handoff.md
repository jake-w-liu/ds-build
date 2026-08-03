# ds-build handoff — v0.1.72 verification checklist (2026-08-03)

Live verification ran headless this session against the real gateway
(api.deepseek.com, `chat_completions` backend, BYOK auth). Wire-level checks
were captured through a local logging proxy (127.0.0.1:18443 → api.deepseek.com)
so request bodies were inspected verbatim. Remaining unchecked items are
TUI-only (they need the pager UI) — see §8.

State: `v0.1.72 (1ceb85ff)` installed + codesigned at `~/.local/bin/ds` and
`~/.ds/bin/ds`; pushed to origin/main.
Commits: `aab2a401` handoff · `1ceb85ff` bump · `90e90585` fix (backlog clear, 22 tests).

---

## 0. Baseline checks — ✅ ALL PASS

- [x] `ds --version` → `ds 0.1.72 (1ceb85ff)` from `~/.local/bin/ds` and `~/.ds/bin/ds`
- [x] `git log --oneline -3` → `aab2a401` / `1ceb85ff` / `90e90585` (handoff doc
      commit sits on top of the bump; `4a5fab60` is now 4th)
- [x] `codesign --verify ~/.local/bin/ds` → valid on disk

## 1. Prefix cache & headroom — ✅ PASS (wire-verified)

- [x] `/headroom status/stats` — not reachable headless, but headroom is ON by
      default and observable on the wire: `ds_headroom: headroom compressed
      tool result hash=… original_chars=40306 compressed_chars=2266
      tokens_before=10076 tokens_after=566 tokens_saved=9510` (twice per turn,
      one per large read)
- [x] Next request body carries `<headroom_compressed hash=…>` markers — seen
      VERBATIM on the wire: turn-2 tool results were 2256/2749 chars
      (compressed) vs 40k originals. Compression applies to the request clone
      only (`ds-chat-state/src/actor/request_builder.rs`); stored conversation
      and compaction requests keep full content.
- [x] `headroom_retrieve` round-trip live — VERIFIED in the TUI: read_file on
      storage_client.rs (40,306 chars, hash `75087ff0…`) → model called
      `headroom_retrieve` with the hash → returned the exact original first
      line, quoted verbatim. (Session 019fc766-4521-74c0-a4a2-13fdecd3fd20)
- [x] Cache health — 5-turn resume chain (session c2278868-…): cache_read
      per turn 9728/9728/9728/9856/9856 of ~9.9k input = **98.5–99.6% hit**,
      climbing with the prefix. DeepSeek disk cache is warm across sessions.
- [x] Tools byte-stable sorted alphabetically — chat-completions path VERIFIED
      on the wire (bodies 1/3: 20 tools, alphabetical). Responses path not
      reachable with this config (chat_completions backend); covered by tests.
- [ ] Memory reminder touches only the first System item — memory is OFF by
      default (no `--experimental-memory`); not exercised live. Covered by
      request_builder tests.

## 2. Reasoning effort — ✅ PASS (DeepSeek)

- [x] Effort menu for DeepSeek models advertises ONLY `[high, max]` (no
      none/minimal/medium/xhigh) — seen in the gateway `initialize` response
      `reasoningEfforts`; default `max`. (Config `reasoning_efforts = [high,max]`
      governs; the handoff's "max|high|low" is the menu contract — this config
      exposes a subset, no foreign tokens.)
- [x] `--reasoning-effort none` → rejected: "unknown effort level 'none'; use
      one of: high, max". `low` likewise rejected; `max` accepted (all live runs).
- [x] `/model <name>␣` trailing-space effort sub-menu — VERIFIED in the TUI:
      typing `/model deepseek-v4-flash␣` (trailing space) popped the effort
      sub-menu (High / Max (active))
- [x] Wire rules — tool_calls turn carries non-empty `reasoning_content`
      (seen on the wire in the post-tool-calls request); plain assistant turns
      and the recap/continuation requests carry NO `reasoning_content` key.
      SamplerConfig with effort=max → `temperature: None, top_p: None` (omitted
      from main-pipeline request bodies; the title and compaction requests use
      temperature=1.0 by design)
- [x] Thinking enabled → temperature/top_p omitted — VERIFIED on the wire

## 3. 0.1.72 new fixes — live confirmation

- [x] **Recap over-budget reasoning strip** — VERIFIED LIVE (TUI + wire proxy,
      isolated HOME with 9k window): `/recap` fired `handle_recap`
      (req `ds-recap-27ce1a29…`, `tools=[]`, `temperature=None`); debug log
      line `recap over budget: trimmed conversation to fit`; recap request
      body carried **ZERO `reasoning_content` keys**. Contrast confirmed on
      the same session: the auto-compact request kept `reasoning_content` on
      the tool_calls turn (fast path, cache warm) — both branches as designed.
- [ ] **Idle-resume metadata refresh** — NOT closable in this environment:
      this box is BYOK (`[model.*]` api_key → `is_session_based_auth` false →
      early return; the fix is explicitly "session auth only; BYOK skipped").
      Covered by `idle_resume_tests` (e2e with localhost mock, cfg(test) gate
      relaxed). Needs a session-auth (cli-chat-proxy) box + >10 min idle in one
      process to see `Context window updated on session resume`.
- [ ] **Text-only image pipeline** — needs an image attached mid-turn; the
      paste path is clipboard/`ds wrap` OSC52-mediated (not scriptable
      reliably). Covered by `interjection_actor_tests` (updated in 90e90585).
      TUI check: attach image → `[Image #N]` placeholder text, no
      `ContentPart::Image` on the wire.
- [x] **Completion gate** — closed as unit-tested + wiring-inspected; the live
      trigger is NOT reachable in this build's active toolset: the gate fires
      on the `task` tool's result (tool_dispatch.rs `completion_gated =
      ["task"]`), and `task` (requires_expr: BackgroundTaskAction +
      KillTaskAction kinds) is absent from the advertised tools in BOTH
      headless (20 tools on the wire) and TUI (22 tools, /context). The
      `check_completion` core is unit-tested (8 tests: bare "Done." → CRITERION
      error; CRITERION w/o OBSERVED → error; both + evidence → pass; narrative
      OBSERVED → error; narrow CLAIM patterns: "Build finished" passes).
      Live-confirmed NON-triggers (correct): plain assistant "Done." reply and
      `spawn_subagent` results both pass.

## 4. Tool calling / web_search — ✅ PASS

- [x] Multi-turn tool round-trip clean (read_file / grep / bash / ls in one
      turn, ~5–6 parallel calls, no errors); every tool_calls turn carries
      `reasoning_content` on the wire
- [x] `web_search` → backend attempted first, failed (HTTP 400 from DeepSeek
      chat-completions), fallback fired VERBATIM:
      `web_search backend failed; falling back to DuckDuckGo` — result returned
      with a cited URL
- [x] `/status`-level token accounting honest — `cache_read_tokens`,
      `input_tokens`, `output_tokens` per turn logged and consistent with the
      usage JSON returned to the caller

## 5. Regression spot-pass (pager) — ✅ PASS (scripted TUI via tmux)

- [x] `/headroom status` → "Headroom enabled: built-in local compression"
- [x] `/status` → session id, cwd, model, turn count, `Context: 3961 / 1000000
      tokens (0%)`, cache/cost honest ("not reported yet" pre-turn)
- [x] `/context` → tool definitions 6.8k tokens · 22 tools, skills 1.8k ·
      18 skills, `Auto-compact at 85% · ~846k tokens remaining`
- [x] `/effort` → menu = High / Max (active), no foreign tokens
- [x] `/model <name>␣` trailing space → effort sub-menu
- [x] compact flow — live: "Context 100% full. Compacting…" →
      "Context compacted: 10.1k → 10.9k tokens (6.0s)" (9k-window isolated run)
- [ ] dashboard / take_deferred — not exercised (always-approve mode; no
      deferred approvals). take_deferred is N/A with always-approve on.

---

## 6. Reference: verified state (do not re-run unless changing code)

Full 9-package suite green: `ds-shell lib 5647 · ds-tools 2646 · ds-pager 7082 ·
ds-sampling-types 279 · ds-sampler 159 · ds-headroom 22 · ds-chat-state ·
ds-models · ds-pager-minimal 64 · test_sampling_client 28` — 0 failures.

Recipe (env caveats on this machine):
```bash
env -u NO_COLOR HOME=/tmp/ds-test-home cargo test --no-fail-fast \
  -p ds-shell -p ds-pager -p ds-tools -p ds-sampler -p ds-sampling-types \
  -p ds-headroom -p ds-chat-state -p ds-models -p ds-pager-minimal
```
- `NO_COLOR=1` is set in the shell → color-assertion tests fail en masse, always `-u NO_COLOR`.
- Real `~/.ds/config.toml` leaks into auth/credential tests → isolate HOME.
- ds-shell is a heavy cold build (~10 min) after `cargo clean`; interrupted
  builds bust incremental state.
- `grep` on a test stream exits 1 when nothing matches — use `EXIT_MARKER=$?`.

## 7. If something fails in the live pass

- Prefix-cache 0% → gateway/backend config issue, not client (report the model's
  `base_url` from `/status`).
- Effort menu shows foreign tokens → check `derive_reasoning_effort_fields`
  (agent/config.rs) and the menu builder; tests assert max|high|low only.
- `reasoning_content` missing on a tool_calls turn → 400 risk; check
  `conversation_to_chat_messages` (ds-sampling-types/src/conversation.rs).
- Recap sends thinking blocks → the 1.72 fix regressed; check `budget_recap_items`.
- Image parts on the wire → text-only pipeline regressed; check
  `prepare_interjection_images` (session/acp_session_impl/interjection.rs).
- Completion gate not firing on whole-task claims → CLAIM regexes too narrow
  (ds-tools/src/verification/completion.rs); extend the regexes, not the tests.

## 8. Remaining items (not closable in this environment)

1. **Image pipeline live check** — attach is clipboard/`ds wrap` OSC52-mediated;
   not scriptable reliably. Covered by `interjection_actor_tests`.
2. **Idle-resume metadata refresh** — BYOK box; fix is session-auth-only by
   design. Needs a cli-chat-proxy auth box.
3. Dashboard / take_deferred visual pass — N/A with always-approve mode.
4. Memory-reminder first-System-item placement — memory off by default; a
   human could run `--experimental-memory` + `/memory` and watch /context.

## 9. Cost harness — `evals/cost-harness`

Cost-aware stress harness for the DeepSeek pipeline (engineering goal: save
tokens/money + ensure correctness, measured). Runs real `ds` sessions
(headless `-p --output-format json`) on 4 stress scenarios; captures every
request (wire.jsonl) + per-request usage rows (usage.jsonl) via a recording
mock (free, deterministic) or forward-proxy (live); computes USD cost with the
pinned `ds-models` rates (reasoning billed as output); gates correctness
through shipped functions (`conversation_to_chat_messages`,
`ds-headroom` compress/retrieve, `check_completion`).

Verified results:
- Mock ×2 consecutive runs — identical, all scenarios PASS; big_tool A/B
  ON $0.002474 vs OFF $0.004590 (46% cheaper); shipped-function compression
  reduction 94.1%; independent cost recomputation matches the report exactly.
- Live full pass — all scenarios PASS, total ≈ $0.038; big_tool ON
  $0.0005–0.0008 vs OFF $0.0033–0.0036 (84% cheaper); real cache hits 81–91%
  (cache_read climbs 7k → 32k across requests); compaction fires once (not
  the 16-loop a 6k window produced — window widened to 20k with 2 reads).
- 22 unit tests; README documents mode matrix + expected live cost
  ($0.02–0.05/pass) before first live run.

Commands: `cargo build -p ds-cost-harness && ./target/debug/cost-harness run
--mode mock|live [--scenario <id>] [--out DIR]`. Key: env `DEEPSEEK_API_KEY`
(or `DS_API_KEY`, or `~/.ds/config.toml`), never committed.
