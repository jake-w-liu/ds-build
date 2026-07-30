# Phases 5 & 6 — Consolidation and library replacement

The work loop. One behaviour at a time, one commit at a time, verified against a
reference that does not move.

## The loop

For each ledger entry, in ranked order. Do not skip steps, and do not run two
entries concurrently in the same files.

**1. Read every copy in full.**
All of them, completely. Not the first two and an assumption about the rest. The
whole point is that they differ.

**2. Characterise the behaviour with tests.**
If the behaviour has no test, write one *now*, against current behaviour,
including the edge cases you found in step 1 — nulls, empties, boundaries,
error paths. Commit these tests **before** changing any implementation, and
confirm they pass against the unmodified code. A characterisation test that was
written after the change proves nothing.

If the copies disagree, write a test per copy documenting what each does. Those
tests are the behavioural diff table in executable form.

**3. Diff the behaviours and propose a survivor.**
Build the table from `references/04-census.md`. Classify every disagreement as
outlier-bug, real-requirement, or dead-variation. Recommend a survivor: usually
the most correct implementation, extended to cover the union, not the longest or
the newest one.

**Stop here and get approval.** Which implementation survives, and what happens to
each behavioural difference, is a human decision.

**4. Implement the survivor.**
Under CRC. The survivor is new production code and gets the full standard:
correct at every boundary, robust across the realistic input range, complete with
real error handling and no placeholders. Where a difference was classified as a
real requirement, make it an explicit parameter with a documented default — not an
implicit behaviour that depends on call site.

**5. Migrate every caller.**
Every one. `grep` is not sufficient on its own: also check dynamic references,
string-keyed lookups, templates, and re-exports. A partially migrated
consolidation leaves both implementations alive and makes the codebase worse.

**6. Delete the copies.**
Only after every caller is migrated. If one caller cannot migrate, do not delete
its copy — record why in the ledger and move on. A stranded copy honestly noted is
better than a forced migration.

**7. Verify.**
Run, in this order:

```bash
<the project's test suite>
python3 scripts/slopfix.py smells --severity blocking --top 50
python3 scripts/slopfix.py measure
```

Then execute the inventory items this entry touches, by their recorded
verification method. Not "the tests pass" — the actual inventory items.

**8. Commit atomically.**
One consolidation, one commit. The message names the behaviour preserved and the
ledger entry:

```
consolidate date formatting into utils/dates (SL-004)

Six implementations merged into format_date(). Behaviour union preserved:
zero-pads years < 1000 (was only in format_order_date), returns "" for None
(was a raise in format_date — latent crash, fixed, approved 2026-07-28).

Callers migrated: 23. Tests added: tests/test_dates.py (11 cases).
Inventory items verified: INV-012, INV-013, INV-031, INV-044.
scc code lines: 41,208 -> 41,036 (-172).
```

That message is the audit trail. Six months from now it is the only record of why
`None` returns `""`.

**9. Update the ledger. Then the next entry.**

## What not to do in this loop

- **Do not fix unrelated bugs.** Log them; fix them later as their own commits
  with the user's agreement. A behaviour-preserving change and a behaviour change
  in one commit means neither is reviewable or revertible.
- **Do not reformat.** Formatting churn hides the real diff and inflates the
  gross-lines-changed number without changing the net. If the project needs a
  formatter, that is one separate commit, run once, early, before any
  consolidation.
- **Do not rename things you are not consolidating.** Same reason.
- **Do not batch.** Ten consolidations in one commit is one un-bisectable change.
- **Do not leave the tree broken between commits.** Every commit builds and
  passes.

## Ordering

Work from the leaves inward:

1. Pure utilities with no dependencies — formatters, validators, converters. Safe,
   well-bounded, and they validate the verification loop before it matters.
2. Shared infrastructure — HTTP client, error mapping, logging, retry, cache.
3. Domain logic and business rules.
4. Request/response layer, routing, middleware.
5. Hand-rolled framework replacement, last, once everything below it is stable.

God functions (category D) get split before whatever is inside them can be
consolidated, and the split is its own commit: pure extraction, no behaviour
change, tests unchanged and passing.

## Replacing a hand-rolled framework

This is where the biggest reductions come from and where the biggest regressions
come from. Extra discipline applies.

**Never swap a framework in one commit.** The sequence is:

1. **Inventory the hand-rolled thing's real behaviour.** Every feature, including
   the accidental ones: its coercion rules, its error messages, its ordering
   guarantees, its handling of missing and null values. This is a mini behaviour
   inventory and it is the acceptance criteria for the replacement.
2. **Present candidate libraries with trade-offs.** Maintenance status, licence,
   dependency weight, breaking-change history, and — most importantly — the exact
   behaviour delta against the inventory from step 1. **The user chooses.** This
   is not your decision: it is a long-lived dependency commitment.
3. **Write an adapter with the hand-rolled interface, backed by the library.**
   Callers do not change yet.
4. **Migrate callers incrementally**, in batches small enough to verify, with the
   affected inventory items executed each time.
5. **Delete the hand-rolled implementation** once no caller reaches it.
6. **Then, optionally, delete the adapter** and let callers use the library
   directly — a separate commit, and often correctly deferred past the end of the
   engagement.

Behaviour deltas that look cosmetic are the dangerous ones: a different error
message that a client parses, a different sort order that a UI depends on, a
different rounding mode in money arithmetic, a validation that now rejects input
that used to be silently coerced. Each of those needs a row in the delta table and
an explicit decision.

## When a consolidation turns out to be wrong

It happens. The copies looked equivalent and were not; the survivor cannot cover
the union cleanly; the migration reveals a caller with an incompatible contract.

Revert the commit, put the entry back on the ledger with what you learned, and
move on. Do not force it by special-casing the survivor into something worse than
the duplication was. Two clean implementations beat one implementation with a flag
that means "behave like the old broken one".

Record abandoned entries in the final report. A ledger entry marked "attempted,
reverted, reason: `PaymentService` depends on the legacy rounding and changing it
needs a product decision" is genuinely useful output.
