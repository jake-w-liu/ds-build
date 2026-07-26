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
