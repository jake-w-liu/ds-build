# Phase 7 — Rewrite protocol

Some modules cannot be repaired incrementally. Editing them into shape costs more
than replacing them and produces something worse than either the original or a
clean rewrite. For those, do not patch — extract what the code actually does,
write that down, get it approved, and build the module again cleanly against the
written spec.

The rule that makes this safe: **you rewrite against an extracted specification,
never against your understanding of the code.** Reading a mess, forming a mental
model, and writing a new module from that model is how behaviour gets lost. The
spec is written down, reviewed, and then the implementation is checked against the
document.

## When a rewrite is justified

All of these should be true. If only some are, consolidate instead.

- Consolidating in place would touch most lines of the module anyway.
- The control flow cannot be followed reliably: deep nesting, many flags, mutated
  shared state, several concerns interleaved.
- The module has no coherent interface — callers reach into its internals.
- Its behaviour is nonetheless *observable* and *enumerable*, so a spec can be
  written and verified.
- The module's boundaries are clear enough that "done" is definable.

**Not** justified because the code is ugly, uses an old idiom, or you would have
written it differently. Those are style preferences and they do not buy a
reduction.

**The decision to rewrite is the user's.** Bring the assessment: what the module
does, why it cannot be repaired, the estimated line delta, the risk, and the
verification plan. Then wait.

## Step 1 — Extract the specification

Read the module completely and write down what it *does*, not what it should do.
Behaviour, not implementation.

Cover:

- **Inputs** — every parameter, its accepted types and value range, its default,
  and what happens with unexpected input. Include the informal accepted types: if
  a parameter is documented as a string but the code accepts a number and coerces
  it, that coercion is behaviour.
- **Outputs** — exact shape, types, field names, ordering. Ordering especially:
  callers depend on it whether or not it was intentional.
- **Side effects** — writes, network calls, cache mutations, emitted events, log
  lines other systems parse, file and lock operations. Note their order, since
  partial-failure behaviour depends on it.
- **Error behaviour** — every raise and every swallow, with the exact condition.
  Note where errors are currently swallowed: that is behaviour a caller may
  depend on, and also a bug.
- **State** — what persists between calls, what is cached, what is global.
- **Concurrency** — is it safe to call twice at once, is it idempotent, does it
  assume single-threaded execution.
- **Boundaries** — empty collections, `None`/`null`, zero, negative, very large,
  unicode, timezone-naive datetimes, duplicate keys.
- **Performance characteristics that callers depend on** — if something is
  currently O(1) and callers rely on it in a loop, that is part of the contract.

Then a separate, explicit list:

**Bug inventory.** Every behaviour you believe is wrong, with: what it does, what
it should do, who could be affected, and a recommendation to **preserve** or
**fix**. Each line needs a decision from the user before the rewrite starts.

This list is not optional and it is not a formality. It is the whole risk surface
of the rewrite. A rewrite that silently fixes bugs is a rewrite that silently
changes behaviour, and "we made it correct" is not a defence when a downstream
system depended on the incorrect output.

## Step 2 — Get the spec approved

Present the spec and the bug inventory. Ask directly:

- Is anything missing or wrong?
- For each bug: preserve or fix? Preserving is a legitimate answer, and often the
  right one — a fix can then be scheduled as its own visible change.
- Which behaviours are critical, where a regression would be severe?
- Is anything here dead, so the rewrite need not implement it?

Write the approved spec to `.slopfix/specs/<module>.md` and commit it. It is the
acceptance criteria and, later, the documentation the module never had.

## Step 3 — Write characterisation tests against the spec

Before writing the new module. Tests derived from the spec, run against the **old**
implementation, and they must pass. This is the step that catches a wrong spec:
if a test derived from your spec fails against the existing code, your spec is
wrong, not the code. Fix the spec and re-approve.

Where a bug was marked "preserve", the test asserts the buggy behaviour, with a
comment recording the decision and its date. Where it was marked "fix", write both
tests: one marked expected-to-fail against the old code, one asserting the correct
behaviour, so the change is visible in the diff.

## Step 4 — Write the new module

Against the spec document, with the spec open. Under CRC — this is new production
code:

- Correct at every boundary the spec enumerates.
- Robust across the realistic input range, with real error handling. No hard-coded
  values that are not inherent. No swallowed errors, even where the old module
  swallowed them, unless that swallow was explicitly marked preserve.
- Complete: no `TODO`, no stub, no partially handled case. If a spec item cannot
  be implemented, stop and say so rather than shipping a placeholder.

Keep the old module in the tree while the new one is built. Do not delete anything
yet.

## Step 5 — Verify against the spec, then switch

1. Characterisation tests pass against the new module.
2. Walk the spec document item by item and confirm each is implemented. Every
   item, not a sample.
3. Where feasible, run both implementations side by side over real inputs and diff
   the outputs — production log data, a captured request set, or a property-based
   generator. This finds what the spec missed, which is the failure mode step 1
   cannot fully prevent.
4. Switch callers over in one commit, keeping the old module present but
   unreferenced.
5. Execute every inventory item that touches this module.
6. Delete the old module in a following commit.

Two commits, deliberately: if the switch causes a problem, reverting one commit
restores a working system, and the old code is still there to compare against.

## Step 6 — Report it

A rewritten module gets its own section in the final report: why it was rewritten,
the line delta, the link to its spec, every bug that was preserved, every bug that
was fixed and when it was approved, and how it was verified. Bug fixes bundled
into a rewrite are behaviour changes, and the user needs them listed in one place
rather than buried in commit messages.
