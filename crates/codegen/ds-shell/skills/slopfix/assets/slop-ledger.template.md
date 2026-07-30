# Slop ledger — <project>

Census taken at commit `<sha>` on `<date>`. Keep this current as work proceeds: it
is the work queue during the engagement and the backlog afterwards.

## Categories

| Code | Category | Notes |
| --- | --- | --- |
| `A` | Duplicated concept | N implementations of one idea. The core work. |
| `B` | Hand-rolled framework | Homegrown ORM/router/validator/date lib. Biggest win, biggest risk. |
| `C` | Dead code | Unreferenced. Check the unused-is-not-unwanted list before proposing deletion. |
| `D` | God function / file | Blocks other work. Split first, as its own commit. |
| `E` | Over-abstraction | Single-impl interfaces, forwarding wrappers, needless layers. |
| `F` | Ceremonial bloat | Defensive checks on impossible states, redundant validation. |
| `G` | Blocking smell | Swallowed error, placeholder, stub. Fix regardless of line saving. |
| `H` | Correctness/test debt | Missing oracle, boundary, property, or differential coverage. |
| `I` | Security/supply-chain debt | Unsafe behavior, secret, invented dependency, license/advisory gap. |
| `J` | Performance/resource debt | Measured algorithm, allocation, latency, task, or resource defect. |
| `K` | Contract drift | Public API, schema, config, docs, migration, or platform mismatch. |

## Risk

| Code | Meaning |
| --- | --- |
| `R1` | Low — pure function, few callers, well tested. |
| `R2` | Medium — several callers, partial test coverage. |
| `R3` | High — request path, critical behaviour, or thin coverage. |

## Status

`open` → `approved` (survivor chosen) → `in progress` → `done` → or `reverted` /
`deferred`, each with a reason.

---

## Ledger

| ID | Cat | Description | Sites | Est. lines | Risk | Inventory items | Status | Commit |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| SL-001 | A | 6 date formatters, all producing `YYYY-MM-DD` with different null and padding behaviour | `src/utils/dates.py:7`, `src/utils/dates.py:14`, `src/utils/misc.py:4`, `src/api/invoices.py:5`, `src/api/client.js:4`, `src/api/client.js:11` | 172 | R2 | INV-140, INV-012, INV-031 | done | `a1b2c3d` |
| SL-002 | B | Hand-rolled query builder duplicating what the ORM already does | `src/db/query.py` (410 lines) | 380 | R3 | INV-001, INV-002, INV-100 | approved | |
| SL-003 | G | `except Exception: pass` swallowing errors in the webhook path | `src/webhooks/stripe.py:88` | 0 | R3 | INV-080 | open | |
| SL-004 | J | `solve!` allocates an intermediate matrix per iteration | `src/solver.jl:88` | 0 | R2 | INV-160 | open | |
| SL-005 | | | | | | | | |

**Est. lines** is code lines expected to disappear *net* — after subtracting the
consolidated implementation and any new tests. An entry whose net is near zero can
still be worth doing for maintainability; say so rather than inflating the estimate.

---

## Behavioural diff tables

One per category `A` or `B` entry, completed **before** the survivor is approved.
This is the artefact that prevents a reduction from becoming a regression.

### SL-001 — date formatting

| Input / condition | `format_date` | `format_order_date` | `formatDate` (js) | Survivor | Decision |
| --- | --- | --- | --- | --- | --- |
| normal date | `2026-7-5` | `2026-07-05` | `2026-07-05` | `2026-07-05` | zero-padding is correct; `format_date` was wrong |
| `None` | raises `AttributeError` | returns `""` | n/a | returns `""` | latent crash — **fixed**, approved 2026-07-28 |
| year < 1000 | `999-01-02` | `0999-01-02` | `0999-01-02` | `0999-01-02` | 4-digit padding |
| naive datetime | assumes UTC | assumes local | assumes UTC | assumes UTC | local was a bug, approved 2026-07-28 |

Every row where implementations disagree is a decision, and every decision is one
of: **outlier bug** (survivor takes the correct behaviour, recorded as an approved
change), **real requirement** (survivor supports both, usually via a parameter), or
**dead variation** (dropped, with explicit approval).

---

## Deferred and reverted

The backlog. This section is a deliverable — it is what whoever continues the work
starts from.

| ID | Cat | Why not done | What it would need |
| --- | --- | --- | --- |
| SL-002 | B | Reverted: `PaymentService` depends on the builder's legacy rounding; changing it is a product decision | Decision on rounding behaviour, then ~2 days |

---

## Running totals

Update as entries close; cross-check against `slopfix measure`.

| | Lines |
| --- | --- |
| Baseline code lines | |
| Estimated available (census) | |
| Removed so far (net, measured) | |
| Remaining to target | |
