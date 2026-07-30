# Behaviour inventory — <project>

Derived from the codebase at commit `<sha>` on `<date>`, then reviewed and
corrected by `<name>` on `<date>`.

**This document is the contract.** Every behaviour listed here must still work when
the engagement ends. It is derived from the code *before* any reduction work and is
never edited to match the new code — that is what makes it a check rather than a
description. Changes to it are only ever additions (a behaviour that was missed) or
explicitly approved removals, each dated and initialled.

## Verification methods

| Code | Method | Notes |
| --- | --- | --- |
| `T` | Automated test | Name the test. Strongest. |
| `C` | Reproducible command | `curl`, CLI invocation, script. Record expected output. |
| `M` | Documented manual check | UI path with expected result. |
| `R` | Code reading only | Weakest. Cannot be claimed as verified at the CRC gate. |

Any `R` row that is business-critical should become a `T` row **before**
consolidation starts. Writing that characterisation test is the cheapest insurance
in the engagement.

## Criticality

| Code | Meaning |
| --- | --- |
| `C1` | Business-critical. A regression is severe: money, auth, data integrity, legal. |
| `C2` | Important. A regression is user-visible and needs a hotfix. |
| `C3` | Minor. A regression is tolerable until the next release. |

---

## HTTP endpoints

| ID | Method + path | Behaviour | Auth | Errors | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| INV-001 | `GET /api/invoices` | Lists the caller's invoices, newest first, 25 per page | session | 401 unauthenticated, 422 bad cursor | `src/api/invoices.py:40` | `T` `test_list_invoices` | C2 | |
| INV-002 | | | | | | | | |

## Pages / screens

| ID | Route | Behaviour | Data needed | Empty + error states | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| INV-020 | `/invoices` | Table of invoices; row click opens detail; CSV export button | `GET /api/invoices` | "No invoices yet" panel; retry banner on 5xx | `src/pages/Invoices.tsx` | `M` see below | C2 | |

## CLI commands

| ID | Command | Behaviour | Exit codes | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| INV-040 | `app import --file F` | Imports invoices from CSV; reports row count on stdout | 0 ok, 1 parse error, 2 file missing | `src/cli/import.py` | `C` | C2 | |

## Background jobs / scheduled work

| ID | Job | Trigger | Behaviour | Failure + retry | Idempotent | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| INV-060 | `send_reminders` | daily 09:00 UTC | Emails unpaid invoices past due | 3 retries, then dead-letter | yes, per invoice per day | `src/jobs/reminders.py` | `T` | C1 | |

## Event handlers / webhooks

| ID | Event | Payload | Side effects | Duplicate delivery | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| INV-080 | `stripe.payment_intent.succeeded` | Stripe PI | Marks invoice paid, writes audit row, emails receipt | must be idempotent by PI id | `src/webhooks/stripe.py` | `T` | C1 | |

## Permission and visibility rules

Consolidation breaks these more often than anything else, and they are the least
likely to have tests. Be exhaustive.

| ID | Rule | Applies to | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- |
| INV-100 | Non-admins see only their own organisation's invoices | all invoice reads | `src/api/scoping.py:18` | `T` | C1 | |

## Data invariants

| ID | Invariant | Enforced where | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- |
| INV-120 | Invoice numbers are unique per organisation | DB unique index + `src/domain/invoice.py:66` | `T` | C1 | |

## Arithmetic and formatting rules

Record these as **worked examples**, not descriptions. This section is where
consolidation of formatters and money handling gets verified.

| ID | Rule | Examples | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- |
| INV-140 | Dates render `YYYY-MM-DD`, zero-padded, in the org timezone | `2026-07-05`; year 999 → `0999-01-02`; `None` → `""` | `src/utils/dates.py` | `T` | C2 | |
| INV-141 | Money rounds half-up to 2 dp, then formats with thousands separators | `1.005` → `1.01`; `1234.5` → `1,234.50` | `src/domain/money.py` | `T` | C1 | |

## Non-functional and domain contracts

Record requirements that users or callers depend on. Put their executable gates
in `.slopfix/quality.json`.

| ID | Characteristic | Contract | Threshold/examples | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- |
| INV-160 | Performance | `solve!` steady-state latency and allocations | p95 ≤ 20 ms; ≤ 2 allocations for representative input | `C` benchmark gate | C2 | |
| INV-161 | Compatibility | Supported Julia versions | current `Project.toml` compat range | `C` CI matrix | C2 | |
| INV-162 | Reliability | Concurrent cancellation releases all tasks/resources | 100 cancel/restart cycles; no live task/file growth | `T` | C1 | |
| INV-163 | Security | No credentials in source/history; dependencies resolve to reviewed identities | secret scan + clean dependency/SBOM gate | `C` | C1 | |

## Integrations

| ID | Service | Calls | Timeout + retry | Failure behaviour | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |

## Feature flags

| ID | Flag | Off behaviour | On behaviour | Real combinations | Implemented in | Verify | Crit | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |

---

## Manual check scripts

Spell out every `C` and `M` row so anyone can run it.

### INV-020 — invoices page

1. Log in as `demo@example.com`.
2. Navigate to `/invoices`.
3. Expect: table with 25 rows, newest first, CSV export button top right.
4. Click a row. Expect: detail drawer with line items.
5. As a user with no invoices, expect the "No invoices yet" panel.

---

## Coverage summary

Update at the CRC gate. This is what the report quotes.

| | Count |
| --- | --- |
| Total behaviours | |
| Verified by automated test (`T`) | |
| Verified by reproducible command (`C`) | |
| Verified by manual check (`M`) | |
| **Unverified** (`R`, or not executed) | |

List every unverified item by ID with the reason. These appear in the final report
verbatim — an unverified behaviour is a known gap, not a rounding error.

## Approved behaviour changes

Every deliberate change to observable behaviour, with the date it was approved.

| Date | Inventory ID | Change | Reason | Approved by |
| --- | --- | --- | --- | --- |
