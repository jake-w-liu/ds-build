<system-reminder>
<goal-state>
Objective: {objective}
Status: Active
Tokens: {tokens} | Elapsed: {elapsed}
</goal-state>

{bail_preface}{plan_pointer}{verifier_gaps}{strategist_note}{reverify_block}Goal NOT complete — continue working. Next step:
{next_step}

Keep your {todo_tool} list current (≥1 `in_progress`, descriptive
`activeForm`). Run targeted tests after every change you make, not
just at the end. Tests must drive the SHIPPED code on the real path — no
hard-coded values, no starting past the thing under test, no
re-implementing it. Save captured test output and artifacts to your
scratch dir {scratch_dir} {scratch_status}, never shared `/tmp/...`;
the plan's `{SCRATCH}` placeholder resolves there. The verifier AUDITS your committed tests and
saved evidence rather than rebuilding them — leave honest proof or you
WILL be refuted.
Before calling `{goal_tool}(completed: true)`, run the
plan's `## Verification plan` steps yourself and confirm the observations
it lists hold — the harness re-checks against those SAME steps each attempt
and inlines any outstanding verifier gaps above. For math/quantitative goals,
`{scratch_dir}/adversarial-math-verify.log` must include exhaustive sections
(equality-checks, dimensional-checks, edge-cases, count-consistency,
tool-transcript) — not a spot-check sample; the harness rejects incomplete logs.
The log must be grounded in a successful foreground `attacker-math` run. If that
verifier fails to spawn, is cancelled, or has no successful recomputation tool
calls, retry it and keep the goal active; never substitute parent-authored
verification for the failed independent run.
</system-reminder>

## Fail-closed claim-ledger verification for math and rendered artifacts

For any mathematical, scientific, derivation, or quantitative deliverable, completion requires verification of the final artifact itself, not merely its boxed conclusions. These rules apply equally when you are the implementer, an `attacker-math` subagent, or a goal skeptic:

1. After the last artifact mutation, inventory every displayed equality, implication, intermediate formula, auxiliary claim (such as an eigenvector, root count, sign, factor, limit, unit, or stability statement), and boxed result. Record a source locator plus a short unique excerpt for every entry. The verification log must contain this claim ledger and a check outcome for every entry; one summary row per problem is not exhaustive coverage.
2. Re-derive and check each ledger entry independently. A correct final answer does not excuse a false intermediate equality or contradictory explanation. Do not verify a formula by copying or algebraically restating the deliverable's own implementation.
3. Verification code must fail closed: use executable assertions or explicit nonzero exits for every mismatch and threshold. Static `PASS`, checkmark, count, dimensional, or edge-case prose is not evidence. A script that can print success when a symbolic expression remains nonzero, a tolerance is exceeded, or an entry was never checked is invalid.
4. The mandatory attacker must read the exact final artifact after its last modification. Any later artifact edit invalidates the prior ledger and log; rerun the attacker against the new artifact. Never accept self-reported totals such as "126 checks" without reconciling them to the source ledger.
5. Keep reproducible verification programs in the workspace. They may receive the required private `{SCRATCH}` log destination through an argument or environment variable, but must not hardcode an ephemeral resolved scratch directory. Create the selected log parent directory and ensure the program remains runnable after the goal scratch directory is removed.
6. Run every task-provided checker, grader, or scoring command when available. Treat any failure or score below the requested threshold as a defect unless the exact rejected rule is independently demonstrated to be inapplicable; do not dismiss a low score as merely format-sensitive without that evidence.
7. For LaTeX or PDF output, use native target syntax rather than raw Markdown, compile in strict failure mode for at least two passes, and treat overfull content, duplicate labels, unresolved references, raw markup tokens, and clipped equations as defects. Render every page after the final compile and inspect the rendered pages, not only the compiler exit code.

A verifier or skeptic must refute completion when the ledger is absent, incomplete, stale, non-failing by construction, or contradicted by any inspected source step or rendered page. Anti-ratchet rules never suppress a newly established mathematical contradiction, stale evidence, non-reproducible checker, or clipped required output.

## Artifact-coupled verification integrity gate (mandatory)

For mathematics, physics, generated reports, and rendered deliverables, a verifier is valid only when it checks the actual final artifact. A correct recomputation that is disconnected from what the artifact says is not evidence.

Before accepting any verifier or declaring completion, enforce all of the following:

1. **Runtime artifact identity.** The verifier must open the final artifact from disk at runtime and print its canonical path and SHA-256 hash. Record the same hash after the last edit. Any artifact edit invalidates all earlier verification and requires every checker, build, render, and inspection to be rerun.
2. **Source-located claim coupling.** Extract each checked equation, numerical value, sign, unit, limit, edge case, and conclusion from the final artifact, identify its source location or stable marker, and compare that extracted claim with an independent derivation or computation. Merely checking a separately retyped correct formula while the artifact contains a different formula is a verification failure.
3. **No false-green constructs.** Reject `check(..., True)`, `assert True`, unconditional pass records, placeholder checks, skipped cases counted as passes, swallowed exceptions, missing dependencies treated as success, and manually written success totals. A prose statement that calls any artifact claim wrong, false, contradictory, stale, unverified, or a finding is a failure regardless of process exit code or a nearby `PASS` label.
4. **Verifier integrity audit.** Inspect every checker source and its complete output. Confirm it reads the named final artifact, validates the current artifact hash, covers the promised claim ledger, has no unconditional passes, and exits nonzero for a deliberately mismatched extracted claim. Independently scan all checker source and output for failure, skip, exception, stale-hash, contradiction, and finding markers; adjudicate every match rather than trusting a summary counter.
5. **Freshness and coverage manifest.** Produce a machine-readable manifest containing the final artifact hash, every required item or section, source locations for its claims, the checker that covered each claim, and all discrepancies. Empty coverage, missing required items, stale hashes, or any unresolved discrepancy block completion.
6. **Native build and render gate.** Run the task-native build from the final source for the required number of passes. Treat errors, undefined references, overfull or underfull boxes, rerun requests, and package or compiler warnings as findings unless the task explicitly documents why a specific message is harmless. Render every page or view and inspect for clipping, overlap, overflow, missing glyphs, blank output, ordering errors, and raw source leakage. Do not call a build clean without quoting the actual warning scan result.
7. **Parent responsibility.** The parent must read the verifier reports, integrity-audit results, coverage manifest, native build log, and rendered output. Exit code zero never overrides contradictory evidence. Fix every artifact or verifier discrepancy and rerun the full fresh gate before reporting success.
