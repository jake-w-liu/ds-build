# Slopfix report — <project>

| | |
| --- | --- |
| Period | `<start>` → `<end>` |
| Baseline commit | `<sha>` |
| Final commit | `<sha>` |
| Counter | `<scc/<version>, slopfix-builtin/2, or slopfix-builtin/2+julia/<version>>` |
| Definition | non-blank, non-comment lines |
| Scope | frozen in `.slopfix/baseline.json` |

## Result

| | Lines |
| --- | --- |
| Baseline code | |
| Final code | |
| Gross removed | |
| Gross added | |
| Gross method | `slopfix-builtin/2-line-fingerprint-diff` |
| **Net removed** | |
| **Reduction** | **N%** |

| | |
| --- | --- |
| Promised reduction | N% (= L lines) |
| Delivered | L' lines |
| **Attainment** | **N% of the committed goal** |

Reproduce:

```bash
git checkout <final-commit>
python3 scripts/slopfix.py measure --strict
```

Gross added is not waste — it is the consolidated implementations plus the
characterisation tests that make the reduction verifiable. Both numbers are shown so
the shape of the work is visible, not just its net.

## Verification

| | Count |
| --- | --- |
| Behaviours inventoried | |
| Verified by automated test | |
| Verified by reproducible command | |
| Verified by documented manual check | |
| **Unverified** | |

**Unverified items**, each with the reason:

| ID | Behaviour | Why not verified | What would close it |
| --- | --- | --- | --- |

Other checks at the final gate:

| Check | Result |
| --- | --- |
| Project test suite | `<pass/fail>`, N tests (was M) |
| `slopfix measure --strict` | `<exit 0 / findings listed below>` |
| `slopfix smells --severity blocking` in touched files | |
| Configured linters | |
| Application builds and starts | |

## Quality assurance

Reproduce:

```bash
python3 scripts/slopfix.py quality-check --run --strict
```

| | |
| --- | --- |
| Quality model | `ISO/IEC 25010:2023` |
| Profile | `<generic/julia>` |
| Config SHA-256 | `<from .slopfix/quality-report.json>` |
| Full or partial | `full` |
| Strict verdict | `<PASS/FAIL>` |

| Characteristic | PASS | FAIL | UNVERIFIED | NOT_APPLICABLE | Evidence/limit |
| --- | ---: | ---: | ---: | ---: | --- |
| Functional suitability | | | | | |
| Performance efficiency | | | | | |
| Compatibility | | | | | |
| Interaction capability | | | | | |
| Reliability | | | | | |
| Security | | | | | |
| Maintainability | | | | | |
| Flexibility | | | | | |
| Safety | | | | | |

Every required `UNVERIFIED` or any `FAIL` blocks a strict pass. Optional
unverified gates still bound claims and must be named here.

## Approved behaviour changes

Everything the user approved that changes observable behaviour. This is the section
to read when something looks different in production.

| Date | Inventory ID | Change | Reason | Approved by |
| --- | --- | --- | --- | --- |

Bugs found and **deliberately preserved** (not fixed, by decision):

| Inventory ID | Behaviour | Why preserved | Date |
| --- | --- | --- | --- |

## What was done

| Ledger ID | Cat | Change | Net lines | Commit | Inventory items verified |
| --- | --- | --- | --- | --- | --- |

Rewritten modules, if any:

| Module | Why rewritten | Net lines | Spec | Bugs fixed | Bugs preserved |
| --- | --- | --- | --- | --- | --- |

## What was not done

The backlog, in priority order. A deliverable in its own right.

| Ledger ID | Cat | Est. lines | Why not | What it needs |
| --- | --- | --- | --- | --- |

## Integrity findings

Every finding `measure --strict` raised, and its disposition. If any was accepted
rather than fixed, the reason is here.

| Finding | Detail | Disposition |
| --- | --- | --- |

## Remaining known problems

Honest state of the codebase, beyond the backlog above.

- Blocking smells still present in untouched files: N (see `slopfix smells`).
- Modules still in poor shape:
- Areas where test coverage is thin enough to make future changes risky:
- God functions not split:

## Guardrails installed

| Artefact | What it does | What fails the build |
| --- | --- | --- |
| `AGENTS.md` / `CLAUDE.md` | Tells the next agent to search before writing; lists the canonical module per concept | nothing (advisory) |
| `<lint config>` | | |
| `.jscpd.json` | Duplication threshold | duplication above N% |
| `.slopfix/quality.json` | Replays the wider quality contract | failed or required-unverified gate |
| `<ci workflow>` | Line ceiling, duplication, blocking smells, quality | configured ratchet failure |

Line ceiling set at **N** lines (final + headroom). Raising it is a deliberate
commit to `<file>`, appropriate when real functionality is added and not as a way
around the check.

Lint rules set below their target level, with the intended direction:

| Rule | Set at | Target | Why |
| --- | --- | --- | --- |

**Limitation, stated plainly:** these slow re-accumulation, they do not prevent it.
An agent that cannot see the whole project will still write duplicate code. The
checks make it visible at review time instead of six months later.

## Warranty

If offered:

**Covered** — behaviour that worked before, is listed in the inventory, and does not
work now. Reported by `<date>`. Fixed at no charge.

**Not covered** — behaviour already broken before the work; behaviour deliberately
changed with recorded approval (see above); behaviour not in the inventory;
regressions from changes made after handover; new features.

## Artefacts handed over

All of it belongs to the user.

- `.slopfix/baseline.json` — the frozen measurement contract
- `.slopfix/behaviour-inventory.md` — the regression checklist, most durable output
- `.slopfix/slop-ledger.md` — work log and backlog
- `.slopfix/quality.json` — reviewed, executable quality contract
- `.slopfix/quality-report.json` — evidence from the final full quality run
- `.slopfix/specs/` — extracted specs for rewritten modules
- `.slopfix/report.md` — this document
- `AGENTS.md` / `CLAUDE.md`, lint configuration, CI workflow
