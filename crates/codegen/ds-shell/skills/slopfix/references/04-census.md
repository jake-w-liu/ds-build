# Phase 4 — Census

Find everything that was built more than once, everything hand-rolled that a
library already does, and everything that is not doing anything at all. Rank it by
lines recoverable against risk. The output is a work queue, not a report.

## Run the tools

```bash
python3 scripts/slopfix.py census --top 40
python3 scripts/slopfix.py smells --top 120
```

`census` reports two independent things, and you need both:

- **Clone groups** — token-identical blocks after identifiers, numbers and strings
  are normalised. Catches literal copy-paste including renamed variables. Tune with
  `--window` (detection granularity, default 60 tokens) and `--min-tokens`. Lower
  both to ~25 on a small codebase; raise them if the output is dominated by
  repetitive data tables.
- **Concept census** — symbols whose *names* say they implement the same idea, even
  when they share no tokens. This is the one that finds fourteen date formatters
  written independently: `formatDate`, `format_order_date`, `formatInvoiceDate`,
  `formatDueDate` all land in one bucket.

`smells` reports what must not survive the reduction: broad exception handlers,
swallowed errors, placeholder implementations, unused imports, god functions.

Expect `census` to take roughly 45 seconds per 100k code lines — it tokenises
every file and hashes every sliding window. Run it once and work from the output;
`smells` and `measure` are much faster.

The third section, *same name defined in multiple files*, excludes single-word
generic operations (`close`, `read`, `parse`) because many classes implement those
by design. Multi-word names surviving that filter — `formatInvoiceDate`,
`buildOrderQuery` — are the ones worth opening.

Then run the language's real linters, which stay authoritative:

```bash
ruff check --select F,E,W,B,SIM,RET,ARG,ERA,PL .   # Python
npx eslint . --max-warnings 0                      # JS/TS
cargo clippy --all-targets -- -W clippy::pedantic  # Rust
go vet ./... && staticcheck ./...                  # Go
npx jscpd --min-lines 8 --min-tokens 50 .          # duplication, second opinion
julia --project=. --startup-file=no -e \
  'using Pkg; Pkg.instantiate(); Pkg.test()'        # Julia tests
```

For a Julia package, add `Aqua.test_all(YourPackage)` to `test/runtests.jl`.
Aqua checks method ambiguities, undefined exports, stale dependencies, missing
compat bounds, type piracy and persistent tasks. JET can find possible dispatch
errors and type instability on concrete call paths, but it is tightly coupled to
the Julia compiler; pin it in the test environment and treat its version/runtime
pair as part of the check rather than installing an arbitrary latest version.

Empirical study of AI-authored commits in the wild found maintainability debt to
be about 89% of introduced issues, with the top offenders by language being: in
Python, broad exception handling, unused arguments, undefined references, access
to protected members, unused imports; in JavaScript and TypeScript, unused
variables and parameters, shadowed outer variables, and block-scoped variable
misuse. Configure the linters to catch exactly those — see
`references/08-guardrails.md`.

## Classify every finding

Each entry in the ledger gets a category, because the categories have different
procedures and different risk:

**A. Duplicated concept.** N implementations of one idea. The core work. Highest
value, and the place where the union rule below matters most.

**B. Hand-rolled framework.** A homegrown ORM, router, validator, date library,
DI container, state manager, retry mechanism. Usually the single largest line
win in the whole engagement, and the highest risk, because behaviour differences
between the hand-rolled version and the library are subtle and pervasive.

**C. Dead code.** Unreferenced functions, unreachable branches, unused exports,
commented-out blocks, orphaned files, feature-flag branches for flags that no
longer exist. Cheap lines, but see the warning below.

**D. God function / god file.** Not directly a line reduction, but it blocks
everything else. Usually split before it can be consolidated.

**E. Over-abstraction.** Slop runs both ways: single-implementation interfaces,
factories that construct one type, wrapper functions that only forward, config
objects with one caller, three layers of indirection to reach one call. Deleting
a layer is a real reduction.

**F. Ceremonial bloat.** Forty-line docstrings on three-line functions,
defensive null checks on values that cannot be null, try/except around pure
arithmetic, re-validation of already-validated input, logging every function
entry. Note that comments are not counted, so docstring bloat is not a line win —
but the defensive-code half of this category is.

**G. Blocking smell.** Swallowed errors, placeholders, stubs. These must be fixed
regardless of whether they save a line, because leaving them violates Robustness
and Completeness.

**H. Correctness or test-oracle debt.** Untested branches, tests that duplicate
the implementation instead of checking outcomes, missing boundary/property/
differential coverage, and inconsistent domain rules. Characterise before
refactoring; do not count new safety-net lines as slop.

**I. Security or supply-chain debt.** Secrets, unsafe defaults, missing
authorization/input validation, invented or unverified packages, stale manifests,
unknown licenses, and unresolved advisories. Use authoritative project tooling;
the bundled smell scanner is not a security scanner.

**J. Performance or resource debt.** Accidental quadratic work, repeated parsing
or allocation, type instability, task/resource leaks, and compilation/latency
regressions. A finding needs a benchmark, profile, allocation measurement, or
resource reproduction before it is confirmed.

**K. Contract drift.** Public API, configuration, schema, migration,
serialization, documentation, example, or platform support that disagrees with
the implementation. Preserve approved compatibility or record the breaking
change separately.

## The union rule

**When N implementations of one concept are merged, the survivor must implement
the union of what those N actually do — never the majority behaviour.**

This is the rule that separates a reduction from a regression. Fourteen date
formatters are almost never fourteen copies of the same function. They are
thirteen copies plus one that handles a timezone, or a null, or a legacy format,
and that difference is load-bearing. A naive merge to "the common case" silently
breaks one screen, and it will not be found for months.

So for every group, before proposing a survivor, produce a behavioural diff table:

| Input / condition | impl A | impl B | impl C | survivor | decision |
| --- | --- | --- | --- | --- | --- |
| `None` | raises | returns `""` | returns `""` | returns `""` | A was a latent crash — fix, note in report |
| naive datetime | assumes UTC | assumes local | assumes UTC | assumes UTC | B was wrong; **user confirmed** |
| year < 1000 | `999-01-02` | `0999-01-02` | `999-01-02` | `0999-01-02` | zero-padding is correct |

Every row where the implementations disagree is a decision, and every decision is
one of three things:

1. **A bug in the outlier** — the survivor takes the correct behaviour, and the
   change is recorded in the report as an intentional fix.
2. **A real requirement** — the survivor must support both, usually via a
   parameter. Do not delete a behaviour because it is inconvenient.
3. **Genuinely dead variation** — no caller depends on it. Drop it, **with the
   user's explicit agreement**, and record it.

You do not get to make choice 1 or 3 alone. Bring the table, recommend, wait.

## Unused is not unwanted

Before proposing any deletion in category C, check all of:

- static callers, including tests;
- dynamic dispatch: reflection, `getattr`, string-keyed registries, decorators,
  dependency injection, `eval`;
- string references in templates, config, SQL, migrations, serialised data;
- the public API surface — is it exported, is it documented, could an external
  consumer depend on it;
- `git log -S '<symbol>'` for whether it was recently added or recently orphaned;
- entry points that are not code: cron definitions, CI workflows, deployment
  manifests, admin scripts.

Then ask. Something can be genuinely unreferenced and still be a feature the user
sells. Deleting a feature is a product decision, not a refactoring decision.

## Ranking

Order the ledger by expected value: lines recoverable, divided by risk.

Work outward from the leaves. Consolidating a low-level utility with twelve
callers is a contained change; consolidating something in the request path touches
everything. Early wins should be small, safe, and fully verifiable, because they
also validate that your verification loop actually works before you rely on it for
something dangerous.

Take blocking smells (category G) in the same files as an early consolidation
rather than as a separate sweep — you are already reading that code, and the fix
belongs in its own commit either way.

## Output

Use `assets/slop-ledger.template.md`, write to `.slopfix/slop-ledger.md`, commit
it. Per entry: ID, category, description, sites with `path:line`, estimated lines
recoverable, risk, which inventory items it touches, status, and the resulting
commit.

Keep the ledger current as you work. It is the record of what was attempted, what
succeeded, and what was abandoned and why — and the abandoned entries belong in
the final report just as much as the successful ones.
