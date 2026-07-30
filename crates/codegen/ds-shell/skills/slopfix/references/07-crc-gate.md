# Phase 8 — The CRC and quality gate

The gate between "I made changes" and "the work is done". It runs twice: after
every consolidation, in a light form, and once at the end, in full. Nothing is
reported as complete until the full gate passes or its failures are stated.

The standard is the one that applies to all code in this engagement:
**Correctness, Robustness, Completeness — with every claim verified by doing, not
by recalling.**

## Per-change gate

After each consolidation or rewrite, before the commit:

```bash
<project test suite>                                        # must be green
python3 scripts/slopfix.py smells --severity blocking --top 50
python3 scripts/slopfix.py measure
python3 scripts/slopfix.py quality-check --run --only <affected-gate> --strict
```

Then, by hand:

- Execute the inventory items this change touches, by their recorded verification
  method.
- Confirm the characterisation tests were committed *before* the implementation
  change and passed against the original code.
- Confirm no caller was missed: search for the deleted symbols, including in
  templates, config, and string-keyed lookups.
- Confirm the diff contains only this change — no reformatting, no drive-by
  renames, no unrelated bug fixes.

A red test suite is a stop, not a note. Do not stack another change on a broken
tree.

## Correctness

For every consolidated or rewritten unit, trace it — do not assume:

- **Boundaries.** Empty collection, single element, maximum size. First and last
  iteration. Off-by-one in every index and slice.
- **Null and empty.** `None`/`null`/`undefined`, empty string, zero, empty object.
  Which of these did each original copy accept, and which does the survivor
  accept? A survivor that raises where a copy returned a default is a latent
  crash at whichever call site relied on it.
- **Error paths.** Every branch that raises or returns an error, executed. Error
  paths are the least-tested code in an AI-written repo and the most likely to
  have been silently changed by consolidation.
- **Types and coercion.** Where the originals coerced informally, does the
  survivor? If it now rejects what used to be coerced, every caller passing the
  loose type is broken.
- **Arithmetic.** Rounding mode, integer versus float division, currency
  precision, timezone and DST, unit consistency. Verify with worked examples, not
  by reading.
- **Ordering.** If any original returned results in a particular order, callers
  may depend on it whether or not it was documented.

## Robustness

- The realistic input range, not the happy path. Malformed input, unexpected
  types, oversized values, concurrent calls where relevant.
- No hard-coded value that is not inherent to the problem. If a constant is
  genuinely inherent, it carries a comment saying why.
- No workaround that only makes code *appear* to work. Specifically none of:
  faked return values, swallowed exceptions, hard-coded expected outputs, tests
  weakened to pass, assertions removed, error cases mapped to a default that hides
  them.
- Resources released on every path including the error paths: files, connections,
  locks, transactions, subscriptions.
- Partial failure handled. If a consolidated function now does three things where
  three functions each did one, what happens when the second fails? That is a new
  failure mode the originals did not have.

## Completeness

- No `TODO`, `FIXME`, `NotImplementedError`, `todo!()`, `pass  # later`, or
  commented-out fallback in code you touched.
- Every case the spec or the inventory names is implemented — not most of them.
- Error handling is real: errors are logged with context or propagated with
  meaning, never discarded.
- Anything you could not finish is stated explicitly in the report. An honest
  "this consolidation was abandoned because X" is a complete deliverable; a stub
  that looks finished is not.

## Final gate

Run all of it. Every failure is either fixed or reported — never quietly dropped.

**1. Behaviour inventory: full pass.**
Every item executed by its recorded method. Record the result per item. Items that
cannot be verified are marked unverified *with the reason*, and they appear in the
report. Do not mark an item verified because the code "looks right".

**2. Test suite: green, and meaningfully larger.**
The suite should have grown — characterisation tests were added for every
consolidation. A suite that shrank is an integrity finding: `measure` reports it,
and it needs an explanation.

**3. Measurement and integrity.**

```bash
python3 scripts/slopfix.py measure --strict
```

Must exit zero, which means every integrity finding is cleared:

- `code-parked-outside-scope` — code moved to an excluded directory instead of
  deleted;
- `code-golf-suspected` / `long-line-introduced` — lines materially longer;
- `comments-stripped` — documentation deleted (which cannot help the count);
- `tests-deleted` — test code shrinking faster than production code;
- `placeholder-introduced` — new stubs or swallowed errors in changed files.

"Cleared" means fixed, or explained in the report with a reason the user accepts.
It does not mean ignored.

**4. Smells: no regressions.**

```bash
python3 scripts/slopfix.py smells --severity blocking --strict
```

Blocking smells in files you touched must be zero. Pre-existing ones in untouched
files are reported as remaining work, not silently inherited.

**5. Linters clean at the level you configured.**
Run the guardrail configuration from `references/08-guardrails.md` and confirm the
build passes under it. Installing a lint rule the codebase fails is not a
guardrail, it is a broken build handed to the user.

**6. Build and start the application.**
Actually run it. A codebase that passes its unit tests and does not boot has not
preserved behaviour.

**7. Full quality contract.**

```bash
python3 scripts/slopfix.py quality-check --run --strict
```

Run without `--only`. Every required gate must be `PASS` or explicitly
`NOT_APPLICABLE` with a defensible rationale; every executed failure must be
fixed. Optional `UNVERIFIED` gates remain visible in the report and bound the
claims you may make. Cite `.slopfix/quality-report.json`.

## Claims you may not make

Every one of these needs evidence in the conversation, or it does not go in the
report:

- "All functionality preserved" — only if every inventory item is verified by
  test, command, or documented manual check. Otherwise: "N of M behaviours
  verified, K by automated test; the remaining L are listed as unverified."
- "No regressions" — only against executed checks. Otherwise: "no regressions in
  the N behaviours verified."
- "Fully tested" / "production ready" — needs coverage evidence and a passing
  suite.
- "Equivalent behaviour" — needs the behavioural diff, and it is a claim about the
  cases you diffed.
- "Faster" / "more efficient" — needs a measurement, not an argument from
  algorithmic reasoning.
- "Secure", "portable", "compatible", or "all AI slop removed" — needs the
  corresponding quality gates. A passing unit suite or zero bundled smells does
  not establish these claims.

State the measured number, the method, and the gaps. A smaller honest number is
worth more than a larger one that does not survive contact with the user's
production traffic.
