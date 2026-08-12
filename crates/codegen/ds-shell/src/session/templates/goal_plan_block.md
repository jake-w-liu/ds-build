A structured plan for this goal is on disk — the source of truth for "done".
Read it first and keep it open.

Plan: {PLAN_PATH}

- Seed todos from the plan's acceptance criteria via {TODO_TOOL} before
  executing.
- If the plan has a `## Task checklist`, work it in order and flip each
  `- [ ]` to `- [x]` in the plan file as you complete it — the harness mines
  the first unchecked box as your next-step nudge, so a stale checklist
  produces stale nudges.
- Execute item by item; when you deviate, append a bullet to the plan's single
  `## Deviations` section — add to that one section; don't start a new one, and
  don't edit the plan's existing items. Keep it TERSE: ONE bullet per deviation
  (what changed + why); not a progress log, so don't restate the plan or dump
  test counts / "all fixed" / "verification re-run" / "superseding" notes there.
- Before claiming completion, run the plan's `## Verification plan` yourself and
  confirm its observations hold. SAVE durable proof: commit real tests that drive
  the shipped code in-repo, and write the captured run output to your scratch dir
  (the one the goal rules name; never shared `/tmp/...`). Fix any missing
  observation before calling the goal complete.
- For `math` goals: spawn a PARALLEL batch of `attacker-math` critics — one
  per requested result/regime/claim (no hard cap; scale the batch to the task,
  `background: true`, then collect all outputs before gating) — and/or use
  direct computation to challenge the final artifact independently. Cover
  every requested result and the acceptance-critical reasoning behind it. Keep
  a private five-gate ledger for `contract-closure`, `derivation-integrity`,
  `evidence-provenance`, `invariant-ledger`, and `state-isolation`; each gate
  must carry a claim-bound observation or a concrete `N/A` reason. Freeze
  authoritative inputs and the frozen goal-start artifact state plus prior-round gaps in
  `{SCRATCH}`, make narrow edits, and recheck every changed dependency. Do not
  expose this workflow ledger in the user-facing artifact unless requested.
