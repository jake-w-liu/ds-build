# ds-build handoff — orchestration deep-debug mission (2026-08-07)

This handoff replaces the previous v0.1.72 verification doc. Its purpose is
NOT to re-verify the past — it is a **live orchestration deep-debug mission**
for the next session, which runs on the updated ds-build (v0.1.82). Treat every
failed observation as a code bug to fix (deep-debug: reproduce, fix, re-verify)
— never as a "report it and move on" item.

## State

- Shipped: `v0.1.82 (86856a2a)` installed + codesigned at `~/.local/bin/ds` and
  `~/.ds/bin/ds`; pushed to origin/main (`6759ec15..86856a2a`).
- Commits: `86856a2a` chore: bump to v0.1.82 · `84faaa66` feat: orchestration
  upgrade · `2e941565` feat: auto-open orchestration panel + remove /workflows
  · `1a68807a` fix: remove emoji from user-facing output and docs.
- Local-only (NOT pushed, test-only, no binary impact): `3e28d2f0` test: goal
  decisions-log persistence round-trip, cap, and auto-resume decision entry.
- Orchestration upgrade contents (what the new session is verifying):
  - Structured phase/attempt/role on the wire for goal subagents
    (SubagentRequest `goal_phase`/`goal_attempt` → SubagentMeta → SubagentSpawned
    → SubagentInfo); workflow panel places agents by structured data, keyword
    sniffer only as fallback.
  - Panel observability: live activity per agent, `[INFRA FALLBACK]` badge,
    decisions history section, `j`/`k` select + `Enter` drill-down into a
    subagent's live scrollback; panel auto-opens when a goal starts.
  - Fail-closed verification is the DEFAULT (`DS_GOAL_FAIL_CLOSED_VERIFICATION`,
    `[goal] fail_closed_verification`).
  - Auto-resume: BackOff/NoProgress/Infra/Budget pauses resume on a new user
    prompt, capped at `GOAL_AUTO_RESUME_CAP` (3) per goal; `UserPaused` and
    `Blocked` never auto-resume.
  - Stop-detector per-pattern precision accounting auto-suppresses patterns
    below `STOP_PATTERN_PRECISION_THRESHOLD` (0.5) after `STOP_PATTERN_MIN_FIRES`
    (4) fires.
  - Soft spawn backpressure: skeptic spawns defer while live subagent token
    burn >= `GOAL_SPAWN_BACKPRESSURE_FRACTION` (0.8) of the live context
    window (bounded 30s wait, no hard caps).
  - Verifier evidence pack carries plan + changed paths + `TEST_OUTPUT_LOCATION`
    (implementer scratch pointer) in one payload.
  - Persisted decisions log (`GoalDecisionKind`: plan_accepted, verdict,
    strategist_advice, auto_resumed, infra_fallback) survives compaction,
    surfaced as `GoalUpdated.recent_decisions`.
  - Slash hygiene: `/workflows` removed; `SkillSlashRewrite` removed (0 refs);
    skill listing unified (announcement + templated-user-message paths are
    byte-identical, budgeted XML).

## Committed unit-test evidence (do not re-run unless changing code)

- ds-shell: goal_tracker **131** (incl. auto-resume eligibility/cap/
  never-UserPaused, decisions round-trip + cap), goal_stop_detector **26**
  (incl. precision suppression), goal_orchestrator **10**, slash_commands
  **85**, goal_classifier **294** (incl. backpressure defer + evidence pack).
- ds-pager: workflow **8** (incl. keyword-free structured-phase tests),
  goal_detail **70**, slash **455**.
- ds-tools: skill_discovery_tracker **109** (incl.
  `unified_renderer_byte_identical_across_call_sites`).
- Captured logs: session SCRATCH (`tests-all.log`, `bump.log`) — the verifier
  audits these; append new evidence there, never to shared /tmp.

## Mission: live orchestration verification (fill every observation)

### M0. Baseline

- [ ] `ds --version` → `ds 0.1.82 (86856a2a)` (both install paths)
- [ ] `git log --oneline -3` → `86856a2a` / `84faaa66` / `2e941565`
- [ ] `codesign --verify ~/.local/bin/ds` → valid on disk
- [ ] `/goal` is advertised (slash menu) and `/workflows` is GONE
- [ ] `/context` skills listing rows render as `<agent_skill>` XML (unified
      renderer), and the skills row token estimate matches the budgeted listing

### M1. Live orchestration run (the core of the mission)

Run a small but real goal that exercises all three phases, e.g.:

```
/goal Add a --dry-run flag to ds-cost-harness that prints the would-run
scenarios without executing them, with unit tests, and verify it
```

Then observe, in order:

- [ ] **Panel auto-opens** when the goal becomes active (no `g`/key press).
- [ ] **Phases render**: Plan (planner subagent) → Execute (worker) → Verify
      (skeptics) appear in the left column with counts.
- [ ] **Structured placement**: the verifier skeptic(s) land under Verify even
      if their descriptions contain no keyword hints (e.g. "quality-gate-1");
      the planner lands under Plan. This is the structured-phase wire check —
      if a role lands in the wrong phase, the wire plumbing regressed.
- [ ] **Attempt numbers**: the second verification round (if the first is
      rejected) shows `(retry N)` driven by structured `goal_attempt`, not
      prose.
- [ ] **Live activity**: running agents show a `· <activity>` suffix
      (Thinking / Running: …).
- [ ] **Decisions history**: the overlay's Decisions section lists
      `plan_accepted` and `verdict` entries (and `infra_fallback` if any).
- [ ] **Drill-down**: `j`/`k` moves the reversed selection across agent rows;
      `Enter` switches to that subagent's live scrollback; Esc returns.
- [ ] **Dismissal persists**: `Esc` closes the panel; a later `GoalUpdated`
      (mid-goal) does NOT reopen it; the NEXT goal reopens it.
- [ ] **Completion**: on Achieved, the overlay shows Complete + the verdict
      entry; `/goal status` reports the goal.

### M2. Conditional behaviors (trigger if cheap; else confirm covered)

- [ ] **Auto-resume**: if a non-user pause occurs (e.g. `update_goal` cap),
      sending any new user prompt resumes the goal (history shows
      `GoalResumed` with detail `auto:N`; Decisions gains `auto_resumed`);
      a `UserPaused` goal must NOT auto-resume.
- [ ] **INFRA FALLBACK badge**: only reachable when the verification harness
      fails (sampler/transport) — if not triggered live, confirm the
      fail-closed default via `DS_GOAL_FAIL_CLOSED_VERIFICATION` resolution
      (agent/config.rs) and the badge render path (goal_detail.rs).
- [ ] **Backpressure**: needs live subagent burn > 80% of window — confirm the
      gate wiring in `run_verification_stage_for_drain` reads the tracker's
      `live_subagent_tokens`/`live_context_window`.

### M3. Slash + listing regression (quick)

- [ ] `/goal`, `/headroom`, `/compact`, `/memory` (if enabled) resolve; a
      skill slash (e.g. `/deep-debug`) still injects `<skill_information>`.
- [ ] The announcement and templated-user-message skill listings are
      byte-identical for the same skill set (covered by the committed
      `unified_renderer_byte_identical_across_call_sites` test — re-run that
      one filter if you want a live confirmation).

## If something fails

- Panel not auto-opening → `session_notification.rs` goal-start hook
  (`goal_started` transition) or the render gate
  (`show_goal_detail && goal_state.is_some()`).
- Wrong phase placement / missing retry → wire plumbing: `SubagentRequest
  goal_phase/goal_attempt` → `handle_request.rs` SubagentMeta →
  `SubagentSpawned` payload → pager `SubagentInfo` → `structured_or_sniffed_*`
  in workflow_panel.rs. Sniffer fallback should never fire for goal subagents.
- Decisions section empty → `record_decision` call sites in goal.rs /
  goal_support.rs / goal_tracker.rs; `GoalUpdated.recent_decisions` cap
  (`GOAL_DECISIONS_WIRE_MAX`).
- Drill-down dead → `Action::WorkflowDrillDown` router arm (matches
  `session.session_id`), or the `j`/`k`/Enter keys in input.rs.
- Auto-resume wrong → `GoalTracker::auto_resume` eligibility classes + cap;
  `maybe_auto_resume_goal` hook in `handle_prompt` (regular, non-synthetic
  prompts only).
- Badge missing on infra failure → `last_classifier_infra_fallback` flag on
  the FailOpenAchieved arm + `build_goal_updated` wire field.

## Evidence discipline

- Append every observation (pass/fail + the command/output that proves it) to
  the session SCRATCH (`tests-all.log` style), and commit any fix with a
  regression test that drives the shipped code on the real path.
- When the mission passes: leave this file's checkboxes filled, commit the
  updated handoff, and (if code changed) run `bump-and-install.sh`.
