# Excluded reductions

Ways to make the number go down that do not make the codebase better. Every one of
these is out of bounds regardless of how much it would help the target, and most
are detected by `slopfix measure --strict`.

The principle: **the number is a proxy for maintainability. Any change that moves
the number without improving maintainability breaks the proxy**, and once the proxy
is broken the whole measurement is worthless.

## Code golf

Packing statements onto fewer lines. Collapsing an `if` into a nested ternary,
chaining what were readable steps into one expression, semicolon-joining
statements, one-lining a loop body.

Zero maintainability gain, negative readability, and it is the most obvious way to
fake a reduction — which is why it is excluded by contract, not just by preference.

*Detected:* `code-golf-suspected` (mean code-line length rose materially),
`long-line-introduced` (longest line grew past 200 characters).

## Deleting comments or documentation

Comments are not counted, so deleting them cannot help the target. Anyone deleting
them is either confused about the definition or working against the point of the
engagement. Docstrings, inline explanations, ADRs, README content: all stay.

The exception, which is not really an exception: a comment that describes code you
deleted goes with the code. A comment that is now *wrong* gets corrected, not
removed.

*Detected:* `comments-stripped` (comment lines falling much faster than code lines).

## Deleting tests

Tests are the safety net for every other change in the engagement. Deleting them to
hit a line target removes the only evidence that the reduction preserved behaviour.

The test suite should **grow** during this work — characterisation tests are added
for every consolidation. Legitimate test reduction exists: when six duplicated
implementations become one, their six near-identical test files become one, and
that is a real reduction. What is not legitimate is losing *cases*. Count assertions
and covered behaviours before and after, not test file lines.

*Detected:* `tests-deleted` (test code shrinking faster than production code).

## Removing error handling

Deleting a `try`/`catch`, dropping a validation, removing a null check, deleting a
retry. This trades lines for production incidents.

Consolidating six inconsistent error handlers into one correct one is exactly the
work. Deleting error handling because the happy path still passes the tests is not.
Note that consolidation makes this easy to do by accident: if copy D validated an
input and the survivor does not, you removed a validation without meaning to. That
is what the union rule in `references/04-census.md` is for.

*Detected:* `placeholder-introduced` (new swallowed errors in changed files); the
CRC gate is the real check.

## Replacing implementations with stubs

Returning a hard-coded value, raising `NotImplementedError`, leaving a `pass`,
faking a result so a test goes green. This is a Completeness violation and it is
the single worst outcome available, because it looks finished.

If a consolidation cannot be completed, revert it and record why. A ledger entry
saying "attempted, reverted, reason X" is a good deliverable. A stub is not.

*Detected:* `placeholder-introduced`; `slopfix smells --severity blocking`.

## Parking code outside the measured scope

Moving source into `vendor/`, `third_party/`, `external/`, a generated-file naming
pattern, or a newly excluded directory. The lines still exist and still have to be
maintained; they have just left the denominator.

*Detected:* `code-parked-outside-scope`. Every hand-added exclusion is
automatically watched, precisely because adding one is the easiest version of this.

## Widening the exclusion list after baseline

Adding `--exclude-dir` or `--exclude-glob` once work has started, or re-running
`baseline --force` on a partly-reduced tree.

The scope is frozen at baseline and `baseline` refuses to overwrite an existing
manifest for this reason. If the scope was genuinely wrong, re-baseline from a
clean checkout of the *original* commit and disclose it in the report.

*Detected:* `measure` replays the frozen scope from the manifest, so a later
exclusion has no effect on the number. Attempting it simply does not work.

## Taking credit for generated or vendored code

Deleting protobuf output, regenerating a client with fewer lines, dropping a
vendored dependency, removing a lockfile. None of it is code anyone maintains by
hand, and none of it counts.

*Prevented:* generated globs are excluded from the denominator at baseline and
again at measurement, so deleting or regenerating them cannot lower the score.

## Minifying, bundling, or vendoring in reverse

Running a minifier over source, committing a bundle in place of modules, or
inlining a dependency to make it disappear from the count. All produce
unmaintainable code and a smaller number.

*Detected:* `code-golf-suspected`, `long-line-introduced`.

## Deleting features

Removing a behaviour is not a reduction unless the user decided to remove the
feature. "Nothing references it" is not sufficient — see the unused-is-not-unwanted
checklist in `references/04-census.md`.

If a feature genuinely should go, that is a product decision, it is recorded as an
approved behaviour change, and the inventory row is marked removed with the date.

*Detected:* the behaviour inventory pass at the CRC gate.

## Reformatting to change the count

Running a formatter that changes line breaking, switching brace style, changing
line-length limits. This moves the count without changing the code.

If the project needs a formatter, run it **once, early, in its own commit, before
any consolidation**, and note the line delta separately from the reduction. Then it
is a transparent one-off rather than a hidden contribution to the target.

## Batching to hide a regression

Landing many changes in one commit so a regression cannot be bisected or reverted
alone. One consolidation, one commit — always.

---

## If the target needs one of these

Then the target was wrong. Say so, report the honest number, and explain the gap.

The frozen baseline, the excluded-reduction list, and the integrity checks exist to
make the number trustworthy rather than large. A number produced by any of the
moves above is worse than no number, because it destroys the user's ability to
trust the measurement — including the parts of it that were real.
