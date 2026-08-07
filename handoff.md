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

> Result (2026-08-07 session): mission PASSED with two real bugs found,
> fixed, regression-tested, and live-re-verified: (1) the workflow panel
> drill-down was a silent no-op for every goal subagent (router arm searched
> `app.agents`, which never contains goal subagents); (2) the goal-detail
> overlay height budget omitted the Decisions section, clipping decisions +
> the commands hint. Both fixed in commit fc729b1b, live-re-verified on the
> fixed binary, and shipped as v0.1.83. Full evidence: `.ds/mission/tests-all.log`
> + `.ds/mission/artifacts/` (captured panel frames, raw PTY streams).

### M0. Baseline

- [x] `ds --version` → `ds 0.1.83 (<bump commit>)` (both install paths;
      was 0.1.82 (86856a2a) at mission start)
- [x] `git log --oneline -3` → bump / `fc729b1b` (fixes) / `e2bf171a`
- [x] `codesign --verify ~/.local/bin/ds` → valid on disk
- [x] `/goal` is advertised (slash menu — live: typing `/goal` shows
      "Set, manage, or check an autonomous goal") and `/workflows` is GONE
      (live scroll + code grep: 0 refs)
- [x] `/context` skills row renders (live "Skills … N skills"); the
      `<agent_skill>` XML listing is the unified renderer — byte-identical
      test re-run PASSED (`unified_renderer_byte_identical_across_call_sites`)

### M1. Live orchestration run (the core of the mission)

Run a small but real goal that exercises all three phases, e.g.:

```
/goal Add a --dry-run flag to ds-cost-harness that prints the would-run
scenarios without executing them, with unit tests, and verify it
```

Then observe, in order:

- [x] **Panel auto-opens** when the goal becomes active (no `g`/key press).
- [x] **Phases render**: Plan (planner subagent) → Execute (worker) → Verify
      (skeptics) appear in the left column with counts — final panel capture:
      `✓ Plan 1/1 │ ✓ Execute 1/1 │ ✓ Verify 3/3`.
- [x] **Structured placement**: wire `SubagentSpawned` carries
      `goal_phase=plan` (planner) and `goal_phase=verify` (3 skeptics); the
      panel places the skeptics under "Verify · 3 agents". Disambiguation
      (structured beats sniffer) covered by committed
      `structured_phase_wins_over_sniffed_keywords` + keyword-free tests.
- [ ] **Attempt numbers**: the second verification round (if the first is
      rejected) shows `(retry N)` driven by structured `goal_attempt`, not
      prose — NOT OBSERVED live (attempt 1 passed: "Attempts: 1/10"); covered
      by committed `structured_or_sniffed_retry` tests (`goal_attempt - 1`).
- [x] **Live activity**: running agents show a `· <activity>` suffix
      (Thinking / Running: …) — frames show
      `⟳ goal plan writer · Waiting for response… deepseek-v4-flash · 0.4s`.
- [x] **Decisions history**: the overlay's Decisions section lists
      `plan_accepted` and `verdict` entries (live panel capture:
      `verdict — Achieved` + `plan_accepted — plan written: …`; wire
      GoalUpdated #14-16 carry recent_decisions=[plan_accepted, verdict] at
      status=complete). Renders thanks to the height-budget fix (fc729b1b).
- [x] **Drill-down**: `j`/`k` moves the reversed selection across agent rows;
      `Enter` switches to that subagent's live scrollback; Esc returns. Was
      DEAD as shipped — fixed (fc729b1b) and live-re-verified on the fixed
      binary (Enter closes the overlay, Esc returns it, second Esc closes).
- [x] **Dismissal persists**: `Esc` closes the panel; a later `GoalUpdated`
      (mid-goal) does NOT reopen it (live: "Verifying" turn status appeared
      with the panel still closed); the NEXT goal / a resumed session
      reopens it (None→Some transition; verified live via session resume).
- [x] **Completion**: on Achieved, the overlay shows Complete + the verdict
      entry (capture: `Status: Complete` + `Last verdict: Achieved` +
      `Attempts: 1/10` + chat `Goal complete — 25m42s end-to-end.`).
      `/goal status` format code-verified (slash_exec.rs); the live-session
      /goal status step was skipped after the run aborted on the scenario's
      prompt-focus `g` (harness artifact, not a product bug).

### M2. Conditional behaviors (trigger if cheap; else confirm covered)

- [x] **Auto-resume**: not triggered live (requires a non-user pause —
      `update_goal` cap / no-progress / infra / budget; no cheap trigger
      without risking the core run). Covered by committed goal_tracker tests
      (131 incl. auto-resume eligibility, cap, never-UserPaused) + the
      `maybe_auto_resume_goal` hook (handle_prompt, regular prompts only).
- [x] **INFRA FALLBACK badge**: not triggered live (verification harness
      never failed). Confirmed by code: fail-closed is the DEFAULT
      (`resolve_goal_fail_closed_verification` `.default(true)`,
      agent/config.rs:2465-2470); FailOpenAchieved sets
      `last_classifier_infra_fallback` (goal.rs:826) + records
      `InfraFallback` decision (goal.rs:829); badge render path
      goal_detail.rs:966-970.
- [x] **Backpressure**: confirmed by code —
      `run_verification_stage_for_drain` → `run_verification_stage_with_backpressure`
      → SpawnBackpressure closure reads tracker `live_context_window` /
      `live_subagent_tokens` vs GOAL_SPAWN_BACKPRESSURE_FRACTION (0.8),
      bounded 30s (goal.rs:1357-1375, goal_classifier.rs:156-200).

### M3. Slash + listing regression (quick)

- [x] `/goal` resolves (live: advertised + a full 3-phase goal ran to
      completion on it); `/headroom`/`/compact`/`/memory` unchanged (no code
      touched); skill slash injection covered by committed tests.
- [x] The announcement and templated-user-message skill listings are
      byte-identical for the same skill set — re-ran
      `unified_renderer_byte_identical_across_call_sites` live:
      `1 passed` on the current tree.

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
- Drill-down dead → `Action::WorkflowDrillDown` router arm: must open the
  owned subagent view (`subagent_sessions`/`subagent_views` lookup +
  `open_subagent_fullscreen`), NOT search `app.agents` — fixed in fc729b1b.
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
