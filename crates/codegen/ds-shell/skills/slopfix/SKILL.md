---
name: slopfix
description: Remediate AI-generated code debt without hiding regressions. Freeze a reduction baseline; inventory behaviour; create an evidence-bearing ISO/IEC 25010 quality contract; find duplication, over-abstraction, dead code, placeholders, and hand-rolled frameworks; consolidate or rewrite behind correctness, security, performance, dependency, API, documentation, reliability, and language-native gates; install ratchets; and report every unverified gap. Use for slopfix, AI-slop or vibe-code cleanup, de-slopping, code reduction, duplicated logic, technical-debt remediation, large Julia cleanup, or refactoring codebases where generated code passes simple tests but remains repetitive, risky, slow, insecure, or hard to change.
metadata:
  short-description: "Remove verified AI code debt without regressions"
user-invocable: true
argument-hint: "[scope, target, and stop point]"
---

# Slopfix

Take a codebase that AI wrote fast and remove verified debt without changing
required behaviour. The deliverable is not a diff. It is **a smaller, better
specified codebase; a behaviour checklist; a replayable quality contract;
guardrails against re-accumulation; and evidence for every claim.**

AI-authored debt is not only duplication. It can include logic defects, weak
tests, insecure or hallucinated dependencies, performance regressions, stale
APIs, resource leaks, documentation drift, and domain errors. Treat duplication
as the reduction opportunity and the wider quality contract as the boundary that
prevents a smaller codebase from merely becoming differently broken.

## Non-negotiables

Read these as hard constraints, not aspirations. Violating one invalidates the
engagement even if the line count drops.

**1. The agent has no vote.**
You execute, measure, propose, and document. A human decides. Never decide alone:
whether two implementations are equivalent; whether a behaviour is safe to drop;
which library replaces hand-rolled code; whether a module is repaired or
rewritten; whether anything unused may be deleted; whether the target was met.
Bring each of those a recommendation with evidence, then wait. You are running a
disciplined engineering process, not exercising judgment you have not earned on a
codebase you did not write.

**2. Behaviour is the contract; the line count is only the score.**
Every behaviour in the inventory must still work at the end. A reduction that
breaks a behaviour is a regression that happens to have fewer lines. If you can
only hit the target by dropping behaviour, you have missed the target — say so.

**3. Measure before you touch anything.**
`slopfix baseline` runs before the first edit. It freezes the counter identity and
the file scope. A number measured against a moving baseline is not a number. Never
re-baseline after work starts.

**4. Reduce by deleting duplication, never by compressing text.**
Excluded, always: collapsing lines, deleting comments or docs, deleting tests,
removing error handling, replacing implementations with stubs, moving code into an
excluded directory, minifying, and widening the exclusion list. See
`${SKILL_DIR}/references/10-forbidden-moves.md`. `slopfix measure` detects most of these and
reports them as integrity findings you must clear.

**5. CRC on every change, no exceptions for small ones.**
- **Correctness** — trace the logic and the edge cases: boundaries, empty and
  null input, off-by-one, error paths. Consolidated code must be right for every
  caller, not the one you read.
- **Robustness** — handle the realistic input range. No hard-coded values that
  are not inherent to the problem. Never a workaround that only makes code
  *appear* to work: no faked results, no swallowed errors, no hard-coded expected
  outputs.
- **Completeness** — production-grade: real error handling, correct resource
  handling, no `TODO`, no placeholder, no silently skipped case. If you cannot
  finish something, say so explicitly rather than leaving a stub.

**6. Verify by doing, never by recalling.**
Read the file. Run the test. Diff the behaviour. "This looks equivalent" is not
verification. Every equivalence claim needs a citation: a test that covers it, or
a line-by-line behavioural diff you actually performed. Label anything unverified
as unverified.

**7. Missing evidence never means pass.**
Represent every quality gate as `PASS`, `FAIL`, `UNVERIFIED`, or
`NOT_APPLICABLE`. `NOT_APPLICABLE` needs a concrete rationale. A missing tool,
unknown domain invariant, unconfigured benchmark, or absent security scanner is
`UNVERIFIED`, never green.

## Why this does not compound errors

The standard objection is that using AI to shrink an AI-bloated codebase is two
rounds of lossy transcoding, and the errors multiply. That objection is correct
for an unstructured pass, and this method is shaped specifically to defeat it:

- Every change is verified against the **behaviour inventory**, which is derived
  from the code *before* any work starts and never edited to match the new code.
  The reference is fixed, so error cannot accumulate against a drifting target.
- Every change is **one behaviour wide** and lands as **one atomic commit**, so a
  regression is bisectable and revertible on its own.
- Consolidation preserves the **union** of behaviours, never the majority vote.
  The copy that differs is usually the one handling a real edge case.
- Nothing merges on an agent's judgment that two things are "the same".

## The workflow

Run them in order. Each reference file is the working detail; this list is the
spine.

**0. Triage — is this worth doing?** → `${SKILL_DIR}/references/01-triage.md`
Assess honestly and be willing to decline. If the codebase has no coherent
behaviour to preserve, or its bloat is genuinely irreducible, say so and stop.
Charging forward on an unsalvageable repo is the failure mode here. Output: a
go/no-go with reasons.

**1. Baseline — freeze the measurement.** → `${SKILL_DIR}/references/02-baseline.md`
`slopfix baseline --target N` before any edit. Non-blank, non-comment lines only,
via `scc` where available. Confirm the scope with the user, then commit to a
reduction target you can defend. Output: `.slopfix/baseline.json`.

**2. Quality contract — expose the blind spots.** → `${SKILL_DIR}/references/11-quality-assurance.md`
Run `slopfix quality-init --profile auto` before production edits. Review every
generated gate and command. For Julia, retain the Julia profile. Resolve review
gates with evidence or an explicit not-applicable rationale; do not weaken
required gates to make strict mode pass. Output: `.slopfix/quality.json`.

**3. Behaviour inventory — build the safety net.** → `${SKILL_DIR}/references/03-behaviour-inventory.md`
Enumerate every behaviour: each page, each endpoint, each CLI command, each job,
each event handler, each permission rule — what it does, what it returns, how it
fails. Derive it from the code, then have the user confirm and correct it. This
checklist is simultaneously your safety net and the user's guarantee. Nothing else
starts until it exists. Output: `.slopfix/behaviour-inventory.md`.

**4. Census — find what was built more than once.** → `${SKILL_DIR}/references/04-census.md`
`slopfix census` and `slopfix smells`, plus the language's real linters. Classify
every finding: duplicated concept, hand-rolled framework, dead code, god
function, swallowed error, placeholder. Rank by lines recoverable against risk.
Output: `.slopfix/slop-ledger.md`.

**5. Consolidate — one behaviour at a time.** → `${SKILL_DIR}/references/05-consolidation.md`
For each ledger entry: characterise the behaviour with a test if none exists,
diff all copies, get the survivor approved, migrate every caller, delete the
rest, run the affected inventory items, commit atomically. Preserve the
behavioural union. Never batch unrelated consolidations into one commit.

**6. Replace hand-rolled frameworks with mature libraries.** → `${SKILL_DIR}/references/05-consolidation.md`
The largest reductions usually come from deleting a homegrown ORM, router,
validator, or date library. Library choice is a human decision: bring options
with trade-offs, licence, maintenance status, and the exact behaviour delta.

**7. Rewrite what cannot be repaired.** → `${SKILL_DIR}/references/06-rewrite-protocol.md`
Some modules are past patching. Do not edit them into shape. Extract what the
code *actually does* (including its bugs, each flagged as preserve-or-fix),
write that down as a spec, get it approved, then write the module cleanly against
the spec. Verify against the extracted spec, not against your memory of it.

**8. CRC and quality gates — earn the claim.** → `${SKILL_DIR}/references/07-crc-gate.md`
Full inventory pass. Full test suite. `slopfix measure --strict` with every
integrity finding cleared. `slopfix smells --severity blocking` clean for code you
touched. Run `slopfix quality-check --run --strict`; a partial `--only` run is
valid for one change but never substitutes for the final full run.

**9. Guardrails — stop it happening again.** → `${SKILL_DIR}/references/08-guardrails.md`
`AGENTS.md` / `CLAUDE.md` that tells the next agent to search before writing;
lint rules targeting the smells AI actually introduces; a duplication check and a
line-count ratchet in CI. Without this, the codebase re-bloats in a month and the
work was rented, not bought.

**10. Report — honestly.** → `${SKILL_DIR}/references/09-reporting.md`
Baseline, current, net removed, percentage, attainment against the promised
target, what you verified and how, what you could not verify, what you left
undone and why. Under-delivering and saying so is a good outcome. Claiming a
number you did not measure is the only unacceptable one.

## Tooling

`${SKILL_DIR}/scripts/slopfix.py` is bundled and needs only Python 3.10+ and the standard
library. Measurement, census, and smell scans do not modify source files.
`quality-init` writes its requested artifact, and `quality-check --run` executes
the reviewed commands in that artifact.

```
python3 "${SKILL_DIR}/scripts/slopfix.py" doctor
python3 "${SKILL_DIR}/scripts/slopfix.py" baseline --target 40
python3 "${SKILL_DIR}/scripts/slopfix.py" baseline --target 40 --counter julia
python3 "${SKILL_DIR}/scripts/slopfix.py" quality-init --profile julia
python3 "${SKILL_DIR}/scripts/slopfix.py" quality-check
python3 "${SKILL_DIR}/scripts/slopfix.py" quality-check --run --strict
python3 "${SKILL_DIR}/scripts/slopfix.py" census
python3 "${SKILL_DIR}/scripts/slopfix.py" smells --severity blocking
python3 "${SKILL_DIR}/scripts/slopfix.py" measure --strict
```

Run `doctor` first. `auto` prefers `scc`; do not use `auto` for a Julia-focused
repository. Pin `--counter julia` so `.jl` files are classified by Julia's own
tokenizer/parser and all other files use the bundled scanner. The identity pins
both layers and the Julia version, and `measure` refuses a different runtime.
Language-specific checks (`Pkg.test`, Aqua and optionally JET for Julia; `ruff`,
`eslint`, `clippy`, `go vet` elsewhere) stay authoritative; the bundled smell
scan only covers patterns that must not survive the reduction.

Treat `.slopfix/quality.json` as executable input because command gates are argv
arrays. Inspect it before passing `--run`. The runner never uses a shell, bounds
every command with a timeout, records output digests by default, detects changes
to protected project/manifest files, and marks missing executables
`UNVERIFIED`. Read `${SKILL_DIR}/references/11-quality-assurance.md` before
configuring it.

## Scope discipline

Default scope is the whole repository. Narrow only when the user bounds it, and
then say what the boundary is and never imply broader coverage. Two other rules:

- **Unused is not the same as unwanted.** Before deleting anything that looks
  dead, check callers, tests, git history, dynamic dispatch, reflection, string
  references, config, and public API surface — then ask. Deleting a feature is a
  product decision.
- **Do not fix bugs you find while consolidating.** Log them. A behaviour-
  preserving change and a bug fix in one commit means neither can be reviewed or
  reverted. Fix them afterwards, as their own changes, with the user's agreement.

## Stop condition

Stop only when every one of these holds:

- the baseline was frozen before the first edit, and `measure` still resolves the
  same counter identity;
- every inventory item has been executed or explicitly marked unverifiable with a
  reason;
- the full quality report represents all nine characteristics, has no failed
  gate, and has no required `UNVERIFIED` gate;
- every change is a separate commit whose message names the behaviour it
  preserves;
- the test suite passes, and new characterisation tests cover every consolidation;
- `measure --strict` reports no uncleared integrity findings;
- no blocking smell was introduced by your changes;
- guardrail artefacts are in place;
- the report states the measured number, the method, and everything unverified or
  undone.

If you cannot reach the target without violating a non-negotiable, stop at the
number you honestly reached and report the gap. That is the correct outcome, and
it is the whole reason the target is measured rather than asserted.
