# Full quality assurance

Use this phase because duplication and line reduction do not cover all defects
introduced by generated code. The quality contract complements the behaviour
inventory; it does not replace it.

## Contract

Create the contract before production edits:

```bash
python3 scripts/slopfix.py quality-init --profile auto
python3 scripts/slopfix.py quality-check
```

`quality-check` without `--run` validates JSON and executes nothing. Inspect every
command before running:

```bash
python3 scripts/slopfix.py quality-check --run --strict
```

The configuration is executable input. Commands are argv arrays, not shell
strings. Do not embed secrets. Keep timeouts finite. Use project-relative working
directories and protected paths.

Every ISO/IEC 25010:2023 product-quality characteristic must have at least one
gate:

- functional suitability;
- performance efficiency;
- compatibility;
- interaction capability;
- reliability;
- security;
- maintainability;
- flexibility;
- safety.

Every result is one of:

| Status | Meaning |
| --- | --- |
| `PASS` | An executed command/static check passed, or a reviewed claim has cited evidence. |
| `FAIL` | A check found a defect or a reviewed claim records a failure. |
| `UNVERIFIED` | No adequate check ran. Missing tools stay here. |
| `NOT_APPLICABLE` | The gate was reviewed and has a concrete rationale for exclusion. |

Strict mode fails on any `FAIL` and on every required `UNVERIFIED` result. Do not
make a gate optional or not applicable merely to turn CI green.

## Gate kinds

- `command`: execute the exact argv array without a shell. Record duration, exit
  status, executable path, and stdout/stderr byte counts and SHA-256 digests.
  Set `"capture": "tail"` only when retaining output is safe.
- `reachable-contains`: a text-based custom gate that follows literal Julia
  `include("...")` spellings and requires marker text in the resulting graph.
- `julia-reachable-contains`: parse Julia syntax, ignore comments and quoted
  expressions, follow literal `include` calls, and require target expressions
  in that structurally reachable graph. Dynamic includes make a missing target
  `UNVERIFIED`. The Julia profile uses this for `Aqua.test_all`; it establishes
  syntactic reachability, not that a conditional path executed.
- `review`: record `pass`, `fail`, `unverified`, or `not_applicable`. Pass/fail
  needs evidence; not-applicable needs a rationale.

The runner rejects absolute/escaping paths, duplicate gate IDs, shell command
strings, unbounded timeouts, unknown categories, and configs that omit a quality
characteristic.

Native Windows command gates remain `UNVERIFIED` because the standard-library
runner cannot guarantee cleanup of descendant processes. Run them under WSL or
replace them with an external CI gate whose process-tree lifecycle is enforced.

## Julia profile

Use `--profile julia`, not generic, for maintained Julia code. The generated
contract includes:

- environment-isolated `Pkg.test()` for standard packages, with configured
  project and manifest paths checked for mutation, or an unresolved
  application-test review gate when no matching
  `src/<ProjectName>.jl` exists;
- dependency resolution/precompilation in a temporary Julia depot;
- coverage and test-strength review;
- numerical/domain cases: NaN, infinities, `missing`, degenerate shapes,
  overflow, precision, units, tolerances, seeded randomness, and trusted
  reference results where applicable;
- BenchmarkTools/PkgBenchmark time, memory, allocation, and latency thresholds;
- supported Julia/OS/architecture and extension matrices;
- Documenter/doctest/example execution;
- Tasks, threads, channels, locks, cancellation, and resource cleanup;
- secrets, SBOM, license, advisory, and dependency provenance review;
- Aqua, optional ExplicitImports, and pinned JET checks;
- public API, method-extension, schema, preferences, deprecation, and
  serialization compatibility;
- an explicit safety/domain-hazard disposition.

The clean-resolution gate uses a temporary environment and depot. For a
standard package it develops the package path into that environment and
precompiles it; for an application it copies the project and available
manifest, converts local dependency `path` entries in those temporary copies to
absolute paths, then resolves and instantiates them. It does not use the user's
Julia depot. Any configured protected project or manifest file changing during
a command is a failure.

JET is version-coupled and path-sensitive. Pin it and name concrete public or hot
entrypoints. Aqua is broad package hygiene, not a substitute for domain tests,
security review, or benchmarks. Julia security-advisory tooling is still
evolving; unavailable coverage must remain `UNVERIFIED`.

## Per-change and final use

Run relevant gates while consolidating:

```bash
python3 scripts/slopfix.py quality-check --run --only julia-tests --strict
python3 scripts/slopfix.py quality-check --run --only julia-benchmarks --strict
```

An `--only` report is marked partial. At the final CRC gate run the entire
contract without `--only`, commit `.slopfix/quality-report.json`, and copy its
status-by-category table into the final report.

Do not claim “all slop removed,” “secure,” “fast,” “portable,” or “correct” from
the absence of failures alone. State exactly which gates passed, which were not
applicable, and which remain unverified.
