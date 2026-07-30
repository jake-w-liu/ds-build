# Phase 9 — Guardrails

Without this phase the work is rented. The codebase re-bloats at the same rate it
bloated before, because nothing changed about how code gets added to it. The
guardrails are part of the deliverable, not a nice-to-have.

Four layers, in increasing order of scope:

1. **Instructions for the next agent** — an `AGENTS.md` / `CLAUDE.md` that tells
   it to search before writing. Cheap, and the least reliable, because it depends
   on the agent reading and following it.
2. **Lint rules** — mechanical, targeted at the smells AI actually introduces.
   Reliable for what they cover.
3. **CI checks** — a duplication threshold and a line-count ratchet. The only
   layer that cannot be ignored, because it fails the build.
4. **Quality contract** — replayable correctness, performance, compatibility,
   documentation, reliability, security, maintainability, flexibility, and
   safety evidence.

Install all four. CI enforces the mechanical layers and the reviewed quality
contract.

## Layer 1 — Agent instructions

Copy `assets/guardrails/AGENTS.md.template` to the repository root as `AGENTS.md`,
then symlink or copy it to `CLAUDE.md` so both toolchains read it. Fill in the
project-specific sections — a generic file is ignored; a file that names *this
project's* canonical modules gets used.

The essential content, in priority order:

- **Search before writing.** Name the exact commands. "Before writing a helper,
  `rg` for existing implementations of the concept and check `src/utils/`."
- **The canonical module for each common concept.** This is the highest-value
  section and the one that prevents formatter number fifteen. A table: dates →
  `src/utils/dates.py`, HTTP → `src/lib/http_client.ts`, money →
  `src/domain/money.py`, and so on. Keep it current.
- **What not to hand-roll**, with the library to use instead.
- **Error handling policy.** No bare `except`, no empty `catch`, no discarded
  errors. State the project's actual convention.
- **Where things go.** The directory layout and what belongs in each part, so new
  code does not land wherever the agent happened to be reading.
- **Definition of done.** Tests required, lint clean, no placeholders.

## Layer 2 — Lint rules

Target the smells that AI-authored commits actually introduce. Empirically, the
top ones are, in Python: broad exception handling, unused arguments, undefined
references, access to protected members, unused imports; and in JavaScript /
TypeScript: unused variables and parameters, shadowed outer variables, and
block-scoped variable misuse. Configure for exactly those first — a maximal rule
set that the team disables in week two is worth less than five rules that stay on.

Starting configurations are in `assets/guardrails/`:

The template syntax was rechecked on 2026-07-30 with Ruff 0.16.0, ESLint 10.8.0,
and jscpd 5.0.14. That proves the files load under those versions, not that a
target repository already passes their rules; introduce them at the level the
project can actually sustain.

- `ruff.slopfix.toml` — Python. Its tables map onto `[tool.ruff]`,
  `[tool.ruff.lint]`, `[tool.ruff.lint.per-file-ignores]`,
  `[tool.ruff.lint.pylint]` and `[tool.ruff.lint.flake8-bugbear]` in
  `pyproject.toml`. Validate it with the project's pinned Ruff version before
  making it required; the `tests/**` ignores intentionally permit internal
  access and magic values in tests.
- `eslint.slopfix.mjs` — JS/TS flat config. Copy to `eslint.config.mjs` or merge
  its blocks in. The TypeScript block is
  **self-guarding**: naming an `@typescript-eslint/*` rule without the plugin
  aborts the entire ESLint run, so the file only adds those rules when
  `typescript-eslint` actually resolves. In a JS project it works as-is; for TS,
  `npm i -D typescript-eslint typescript` activates the typed block. The
  type-aware rules (`no-floating-promises`, `await-thenable`, …) need type
  information and are left commented out with the `projectService` wiring ready.
- `jscpd.slopfix.json` — duplication threshold. Validate it with the project's
  pinned jscpd major version and confirm the command exits non-zero above
  `--threshold` before relying on it in CI. The template includes Julia's
  `julia` format (`.jl`) as well as the other source languages.

For other languages:

```bash
cargo clippy --all-targets -- -D warnings          # Rust
go vet ./... && staticcheck ./...                  # Go
```

For Julia:

```bash
julia --project=. --startup-file=no -e \
  'using Pkg; Pkg.instantiate(); Pkg.test(; coverage=true)'
```

Put `Aqua.test_all(YourPackage)` in `test/runtests.jl` so it runs through the
normal package test gate. Add JET only after pinning a Julia-compatible version
and clearing or documenting its existing findings; JET is compiler-coupled and
its results can change with the Julia runtime. The supplied CI template reads a
`+julia/<version>` counter identity and installs that exact runtime before
`measure`, so a parser-backed baseline remains replayable.

**Introduce them at the level the codebase currently passes, then ratchet.**
Turning on a rule that produces 4,000 errors means the rule gets removed. Fix
what you can inside the engagement, set the threshold at where you left it, and
document the intended direction.

## Layer 3 — CI checks

`assets/guardrails/slopfix-ci.yml` is a GitHub Actions workflow; adapt it to
whatever CI the project uses. It runs four things:

**1. Line-count ratchet.** Fail the build if in-scope code lines exceed a recorded
ceiling. This is the check that makes the reduction durable: the codebase can
change freely but cannot grow past the line you left it at without someone
explicitly raising the ceiling.

```bash
python3 scripts/slopfix.py measure --strict
```

Set the ceiling with a little headroom — a few percent — so ordinary feature work
is not blocked, and require a deliberate commit to raise it. The point is that
growth becomes visible and intentional, not that it becomes impossible.

**2. Duplication threshold.**

```bash
npx jscpd --config .jscpd.json --threshold 3 .
# or, with no Node toolchain:
python3 scripts/slopfix.py census --json > /tmp/census.json
```

Fail when the duplicated share exceeds the level you left. Same ratchet logic.

**3. Blocking smells.**

```bash
python3 scripts/slopfix.py smells --severity blocking --strict
```

Zero new placeholders, swallowed errors, or stubs. This one should be strict from
the start; there is no legitimate reason to add a swallowed error.

Keep `.slopfix/baseline.json` committed so CI can measure against it, and note in
the workflow that the manifest is the contract artefact and must not be
regenerated casually.

**4. Full quality contract.**

```bash
python3 scripts/slopfix.py quality-check --run --strict
```

Commit `.slopfix/quality.json` only after reviewing its commands. It is executable
input. Strict mode fails on every failed gate and every required unverified gate.
A missing executable remains `UNVERIFIED`, never pass. An `--only` run is useful
during one consolidation but cannot replace the full final/CI run.

The workflow also needs the counter implementation. Copy
`scripts/slopfix.py`, `scripts/slopfix_lib/`, and `scripts/julia_lines.jl` from
the skill into the target repository's `scripts/` directory as one unit. The
launcher is not standalone: copying only `slopfix.py` leaves its imports missing,
and a Julia baseline also needs the helper beside it. Commit these files so CI
replays the same implementation that wrote the baseline.

## Handover

The guardrails only survive if the user knows what they are. Include in the report:

- what each check does and what fails the build;
- how to raise the line ceiling deliberately, and when that is legitimate;
- how to add a new canonical module to the `AGENTS.md` table;
- which lint rules were set below their ideal level, and what the target is;
- how to update quality-gate evidence and why missing tools cannot be waived
  silently;
- the honest limitation: these slow re-accumulation, they do not prevent it. An
  agent that cannot see the whole project will still write duplicate code. The
  checks make it visible at review time instead of six months later.
