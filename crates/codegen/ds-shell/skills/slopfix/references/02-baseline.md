# Phase 1 — Baseline

The reduction number is the engagement's headline claim, so the measurement has to
be pinned before the first edit and replayable afterwards. Everything in this
phase exists to make one sentence defensible: *"this codebase went from X to Y
code lines, counted the same way both times."*

## The definition

**Code lines = non-blank, non-comment lines.** Nothing else counts.

This definition is doing real work. Because comments are excluded, deleting them
cannot improve the number — which removes the incentive to strip the
documentation. Because blanks are excluded, reformatting cannot improve it either.
The only way the number moves is if code goes away.

`scc` is the canonical counter: its `Code` column is this definition, and it is
what the engagement quotes.

```bash
scc --by-file -f json --no-cocomo .
```

If `scc` is not installed, install it (`brew install scc`, or
`go install github.com/boyter/scc/v3@latest`). The bundled counter implements the
same *definition*, but it is a different counter with its own identity;
`slopfix measure` refuses to compare across identities, because a difference of
two counters is not a reduction.

### Choose the counter before quoting a target

`auto` prefers `scc` when it is installed. Use it only when that is the policy you
intend.

For a Julia or mixed Julia repository, pin:

```bash
python3 scripts/slopfix.py baseline --target 40 --counter julia
```

This classifies `.jl` files with Julia's own tokenizer/parser and uses the bundled
scanner for other languages. It needs `julia` on PATH at baseline and measure
time. The identity records the bundled-scanner version and Julia version, for
example `slopfix-builtin/2+julia/1.12.6`; a different or missing runtime is
refused instead of silently falling back.

The parser-backed path matters because Julia syntax is not safely classified by
quote heuristics alone: docstrings precede definitions, ordinary strings may span
lines, `#= =#` comments nest, `'` can mean either a character or adjoint, and a
docstring plus definition may share one physical line. The bundled regression
suite covers those cases and an end-to-end Julia CLI fixture. That is strong
fixture evidence, not a claim that every possible Julia program has been proven.

Use `--counter builtin` for a dependency-free heuristic count, or `--counter scc`
when an existing contract specifically requires an scc number. `baseline`
cross-checks non-builtin counters against the bundled scanner and warns when they
differ materially. Investigate and disclose that warning before accepting the
target.

The bundled scanner's known limits are explicit: JavaScript regex-versus-division
and Julia character-versus-adjoint decisions use token-position heuristics, while
Julia docstring semantics require the parser-backed counter. Scan warnings name
the file and line instead of failing silently.

## Freeze it

```bash
python3 scripts/slopfix.py baseline --target 40
```

This writes `.slopfix/baseline.json` containing:

- the counter identity (`scc/<version>`, `slopfix-builtin/2`, or
  `slopfix-builtin/2+julia/<version>`);
- the git HEAD it was taken at;
- the exact scope: excluded directories, excluded globs, generated-file globs,
  whether config/data/prose languages count, the file-size cap;
- per-language and per-file code counts;
- ordered hashes of builtin-classified code lines, used to distinguish gross
  deletions, insertions, and replacements without storing source text;
- comment counts, test-code lines, out-of-scope source lines, and code-line-length
  statistics — the inputs to the integrity checks;
- the promised reduction target and the resulting target line count.

Commit it. `.slopfix/baseline.json` is the contract artefact; it should be in the
repository, not in someone's home directory.

**Never re-run `baseline` after work starts.** The command refuses to overwrite an
existing manifest without `--force`, and `--force` is only ever correct before the
first edit. If the scope was genuinely wrong, re-baseline from a clean checkout of
the original commit, and say in the report that you did.

## Getting the scope right

The scope is the denominator, so agree it explicitly before freezing. Defaults:

| Included | Excluded |
| --- | --- |
| Source languages (Python, Julia, JS/TS, Rust, Go, Java, C/C++, Ruby, PHP, Swift, Kotlin, SQL, shell, …) | Config and data (YAML, TOML, JSON), prose (Markdown, RST, text) |
| First-party code anywhere in the tree | Dependency trees, build output, tool caches |
| Tests | Generated files: `*.min.js`, `*_pb2.py`, `*.pb.go`, `*.g.dart`, lockfiles, snapshots |

Two defaults worth confirming with the user:

- **Tests are in scope.** They are real code with real maintenance cost, and
  duplicated test setup is some of the worst slop in an AI-written repo. But
  deleting tests to hit a target is forbidden, and `measure` flags test code
  shrinking faster than production code.
- **Generated code is out of scope.** Nobody is paid to delete protobuf output.
  If a large share of the repo is generated, say so during triage — it changes
  what a percentage means.

Adjust with `--exclude-dir`, `--exclude-glob`, `--include-non-source`. Every
hand-added exclusion is recorded in the manifest and is automatically watched for
parked code, because adding an exclusion is the easiest way to fake a reduction.

## Sanity-check the baseline before committing to a target

Read the output. It should not surprise you.

- Does the file count match roughly what `git ls-files` reports for source
  extensions? A large gap means the scope is wrong.
- Is any single file an implausible share of the total? That is usually generated
  output that escaped the globs, or a vendored dependency.
- Are the top languages the ones you expect?
- Are there parse warnings? The builtin counter reports files it could not scan
  cleanly. A handful in a large repo is normal, all of them concentrated in one
  file is worth a look, and hundreds means the scope has picked up something that
  is not source.

Then check the target is arithmetically survivable: `lines_to_remove` in the
manifest is the concrete number of code lines that must disappear. Compare it to
the census's removable-line estimate. If the target needs more lines than the
census can find, the target is wrong — renegotiate now, not at the end.

## Re-measuring

```bash
python3 scripts/slopfix.py measure --strict
```

Replays the frozen scope and counter and reports baseline, current, gross removed,
gross added, net, percentage, and attainment against the promised target, plus any
integrity findings. `--strict` exits non-zero when findings exist, which makes it
usable as a CI gate.

Run it after every consolidation, not just at the end. A step that removes fewer
lines than expected, or adds more than expected, is worth understanding while the
change is still fresh.

## Attainment

Payment-relevant, and simple: attainment is the fraction of the *promised
reduction* actually delivered.

Promise 50% of 100,000 lines → 50,000 lines must go. Deliver 20,000 → that is 40%
of the goal, not 20%. Over-delivery caps at 100%. Code growth is 0%.

Report both numbers — the reduction percentage and the attainment percentage —
because they answer different questions: what happened to the codebase, and
whether the commitment was met.
