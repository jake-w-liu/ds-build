# Phase 10 — Reporting

The report is what the user is actually buying, alongside the smaller codebase. It
has to be honest enough that someone can audit it, and specific enough that
whoever maintains this code next can use it.

Use `assets/final-report.template.md`, write to `.slopfix/report.md`, commit it.

## The headline

Both numbers, always:

- **Reduction** — what happened to the codebase. `41,208 → 26,940 code lines, net
  14,268 removed, 34.6%`.
- **Attainment** — whether the commitment was met. Promised 40% of 41,208 = 16,483
  lines; delivered 14,268 = **86.6% of the goal**.

State the counter identity and the definition alongside them, so the number is
reproducible: `scc/3.6.0`, non-blank non-comment lines, scope frozen in
`.slopfix/baseline.json` at commit `abc1234`.

Report gross removed and gross added separately from net. Net alone hides the shape
of the work: removing 18,000 lines and adding 3,700 of consolidated implementations
and tests is a different story from removing 14,300 and adding nothing, and the
first is usually the healthier one. The CLI derives gross churn from ordered
code-line fingerprints produced by the bundled scanner and reports that method
separately; the headline baseline/current/net values still come from the frozen
contract counter.

## What must be in it

**Verification, itemised.** The inventory pass, by category:

> 142 behaviours inventoried. 138 verified: 96 by automated test, 31 by
> reproducible command, 11 by documented manual check. 4 unverified: INV-088,
> INV-091 (Stripe webhook paths — no sandbox credentials available), INV-114,
> INV-115 (admin bulk export — requires production data volume).

Not "all functionality verified" unless it is literally true for every item.

**Every behaviour change, in one place.** Anything the user approved that changes
observable behaviour: bugs fixed during consolidation, bugs deliberately preserved,
dead variations dropped, error messages changed, ordering changed. Each with the
date it was approved. This section is the one the user will come back to when
something looks different in production, and burying these in commit messages is
not acceptable.

**Integrity findings and their disposition.** Every finding `measure --strict`
raised, and how it was resolved. If any was accepted rather than fixed, say which
and why.

**What was not done.** Ledger entries attempted and reverted, with the reason.
Entries never attempted, with why — out of time, too risky, needs a product
decision. This section is a genuine deliverable: it is the prioritised backlog for
whoever continues.

**Remaining known problems.** Blocking smells in untouched files. Modules that are
still bad. Places where the test coverage is thin enough that future changes are
risky. The god functions you did not get to.

**The guardrails.** What was installed, what fails the build, how to change the
ceilings deliberately, which lint rules are below their target level.

**The full quality matrix.** Cite `.slopfix/quality-report.json`, its configuration
SHA-256, whether the run was full or partial, and PASS/FAIL/UNVERIFIED/
NOT_APPLICABLE counts for all nine characteristics. Name every optional
unverified gate because it bounds the claims even when strict mode passes.

**Reproduction instructions.** The exact commands to re-derive the number:

```bash
git checkout <final-commit>
python3 scripts/slopfix.py measure --strict
python3 scripts/slopfix.py quality-check --run --strict
```

## Under-delivery

If the target was missed, report the number you reached and explain the gap
concretely: what the census promised, what turned out not to be reducible, and
what a further engagement would need.

Missing a target and saying so is a correct outcome. Every mechanism in this method
— the frozen baseline, the excluded reductions, the integrity checks, the
inventory — exists to make the number honest rather than to make it large. A
reduction that only exists because tests were deleted and code was parked in
`vendor/` is worth less than nothing, because it also destroyed the user's ability
to trust the measurement.

If the target was missed because hitting it would have required violating a
non-negotiable, say that explicitly. That is the system working.

## Warranty scope

If a warranty period is offered, define it precisely, because the ambiguity is
where disputes live:

**Covered:** behaviour that worked before the engagement, is listed in the
inventory, and does not work after it.

**Not covered:** behaviour that was already broken and is still broken; behaviour
deliberately changed with recorded approval; behaviour not in the inventory;
regressions from changes made by others after handover; new features.

The inventory is what makes this line drawable at all. Without it, every dispute is
an argument about what the software used to do, and nobody can win it.

## Handover conversation

Walk the user through, in this order:

1. The two numbers and how to reproduce them.
2. The inventory — this is now their regression checklist, and it is the most
   durable thing they got.
3. Every approved behaviour change.
4. The guardrails and what will now fail their build.
5. The quality matrix, including every not-applicable rationale and unverified
   boundary.
6. The backlog of what was not done, in priority order.
7. The unverified items, and what it would take to close them.

Then hand over the artefacts: `.slopfix/baseline.json`,
`.slopfix/quality.json`, `.slopfix/quality-report.json`,
`.slopfix/behaviour-inventory.md`, `.slopfix/slop-ledger.md`,
`.slopfix/specs/`, `.slopfix/report.md`, `AGENTS.md`, the lint configs, and the
CI workflow. All of it belongs to the user.
