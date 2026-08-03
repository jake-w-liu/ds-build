# ds-build handoff — v0.1.72 verification checklist (2026-08-03)

This session's job: **start `ds` on 0.1.72 and run the live checklist below** to
confirm everything landed. The code-level verification is DONE (full suite
green, see §6); what remains is LIVE verification against the real gateway,
which needs a human-driven interactive session.

State: `v0.1.72 (1ceb85ff)` installed + codesigned at `~/.local/bin/ds` and
`~/.ds/bin/ds`; pushed to origin/main; `cargo clean` done (target/ empty).
Commits: `1ceb85ff` bump · `90e90585` fix (backlog clear, 22 tests).

---

## 0. Baseline checks (~2 min)

- [ ] `ds --version` → `ds 0.1.72 (1ceb85ff)`; same from `~/.local/bin/ds` and `~/.ds/bin/ds`
- [ ] `git log --oneline -3` → `1ceb85ff` / `90e90585` / `4a5fab60`
- [ ] `codesign --verify ~/.local/bin/ds` → valid on disk

## 1. Prefix cache & headroom (0.1.71 core — cache health)

- [ ] `/headroom status` → enabled; `/headroom stats` → segments/tokens saved
- [ ] Run a command with a large tool result (e.g. read a big file) → the next
      request body shows `<headroom_compressed hash=...>` markers (watch with
      `RUST_LOG=debug` or the pager's `--debug` view)
- [ ] `headroom_retrieve` with the hash returns the exact original content
- [ ] `/status` across ≥5 turns → `cached_read_tokens` climbs and cache-hit %
      is > 0. **If it stays ~0% the gateway isn't caching — sorting/headroom
      can't help; check the model's backend config (base_url) instead.**
- [ ] Request bodies show tools byte-stable sorted alphabetically (function
      before hosted) on BOTH the chat-completions and Responses paths
- [ ] Memory reminder only touches the first System item (prefix-stable)

## 2. Reasoning effort (DeepSeek-only)

- [ ] `/effort` menu lists `max|high|low` ONLY (no none/minimal/medium/xhigh);
      default `max` when unmarked
- [ ] `/effort max` applies; `none` is rejected on models whose menu omits it
- [ ] `/model <name>␣` (trailing space) → effort sub-menu; `/model <name> max` works
- [ ] With `--debug`: tool_calls turns carry `reasoning_content` (empty string
      accepted, missing key → 400 per DeepSeek); **plain assistant turns must
      NOT carry it** (0.1.71 wire rule, 1.72 test updated to match)
- [ ] Thinking enabled (`low|high|max`) → temperature/top_p omitted from requests

## 3. 0.1.72 new fixes — live confirmation (the important part)

- [ ] **Recap over-budget reasoning strip**: run a long session past the
      auto-compact threshold. With `--debug`, the recap/compaction request must
      contain NO thinking blocks (over-budget branch force-strips; fast path
      keeps reasoning for the prefix cache). Fix:
      `session/helpers/session_recap.rs` `budget_recap_items`.
- [ ] **Idle-resume metadata refresh**: leave the session idle > 10 min, resume →
      debug log `Context window updated on session resume` and the config
      refreshes from `/models-v2` (session auth only; BYOK skipped). Fix:
      `session/acp_session_impl/session_setup.rs`
      `maybe_refresh_model_metadata_on_resume` (test-build-only gate relax).
- [ ] **Text-only image pipeline**: attach an image mid-turn → NO
      `ContentPart::Image` on the wire; model-visible text keeps the
      `[Image #N]` placeholder (+ drop note if transcription unavailable).
      DeepSeek has no vision — images are transcribed, never sent.
- [ ] **Completion gate**: end a turn with a bare `Done.` claim → gate error
      demanding `CRITERION:` + `OBSERVED:`; add both with real evidence
      (URL / exit code / test count / file:line / code block) → accepted.
      Narrow CLAIM patterns: only whole-task claims gate (sub-step updates
      like "Build finished" must NOT trigger).

## 4. Tool calling / web_search

- [ ] Long multi-turn session (≥5 turns): tool calls round-trip cleanly
      (read_file, grep, bash); each tool_calls turn carries `reasoning_content`
- [ ] `web_search` → hits the configured Responses backend first; on fallback the
      log line `web_search backend failed; falling back to DuckDuckGo` appears
- [ ] `/status` and `/context` stay honest (total_tokens rewrite on terminal
      Responses events)

## 5. Regression spot-pass (pager, light)

- [ ] `/status`, `/context`, dashboard, take_deferred, `/model` switch,
      `/effort`, compact flow — the pager's 7082 tests cover these; a quick
      manual pass on the running TUI is enough.

---

## 6. Reference: verified state (do not re-run unless changing code)

Full 9-package suite green this session:
`ds-shell lib 5647 · ds-tools 2646 · ds-pager 7082 · ds-sampling-types 279 ·
ds-sampler 159 · ds-headroom 22 · ds-chat-state · ds-models ·
ds-pager-minimal 64 · test_sampling_client 28` — 0 failures anywhere.

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
