# Phase 3 — Behaviour inventory

The inventory is the single most important artefact in the engagement. It is what
makes the reduction verifiable rather than hopeful, and it is what stops two
rounds of AI editing from compounding errors: it is derived from the code *before*
any work starts, and it is never edited to match the new code.

**No consolidation begins until the inventory exists and the user has confirmed
it.** This is not process for its own sake — without it you have no definition of
"did not break anything", and every later claim is unfalsifiable.

## What goes in it

One row per observable behaviour. "Observable" means someone or something outside
the code can tell whether it happened.

Enumerate all of these that apply:

- **HTTP endpoints** — method, path, auth requirement, request shape, success
  response, each error response and its status code.
- **Pages / screens / routes** — what renders, what data it needs, what the user
  can do there, what empty and error states look like.
- **CLI commands** — flags, arguments, stdout shape, exit codes.
- **Background jobs / cron / queue consumers** — trigger, schedule, idempotency,
  what happens on failure and on retry.
- **Event handlers / webhooks / subscriptions** — event, payload, side effects,
  ordering and duplicate-delivery assumptions.
- **Scheduled reports, emails, notifications** — trigger condition, recipients,
  content.
- **Permission and visibility rules** — who can see and do what. These are the
  most commonly broken by consolidation and the least likely to be tested.
- **Data invariants and migrations** — uniqueness, referential integrity,
  computed fields, soft deletes, audit trails.
- **Integrations** — third-party calls, their failure handling, their retry and
  timeout behaviour.
- **Feature flags** — what changes when each is on or off. A flag doubles the
  behaviour space; note which combinations are real.
- **Money, tax, and time arithmetic** — rounding, currency, timezone, DST, and
  cutoff rules. Write these down as worked examples, not descriptions.
- **Non-functional contracts** — latency, throughput, allocation/memory,
  supported runtimes/platforms, API/schema compatibility, reliability,
  concurrency/resource cleanup, security, accessibility, and domain safety.
  Record only real commitments and connect each to `.slopfix/quality.json`.

## How to derive it

Derive from the code, then have the user correct it. Both halves matter: the code
tells you what the system *does*, the user tells you what it is *for*, and the gap
between them is where the risky decisions live.

Read the route table, the CLI parser, the job registry, the event subscriptions,
the migration history, the test suite, and any API documentation. For each entry
record where it is implemented — you will need that in phase 5 to know which
inventory items a change touches.

Then present it and ask specifically:

- Is anything missing?
- Is anything on this list dead — shipped but not actually used?
- Which of these are business-critical, where a regression would be severe?
- Where is current behaviour wrong, i.e. a bug you want preserved for now rather
  than fixed silently?

That last question matters more than it looks. Consolidating fourteen date
formatters means picking what the survivor does, and if one of them has been
quietly producing the wrong format in one screen for six months, you need to know
whether fixing it is welcome or is itself a regression.

## Recording it

Use `assets/behaviour-inventory.template.md`. Write to
`.slopfix/behaviour-inventory.md` and commit it.

Each row needs: a stable ID, the behaviour, where it lives, how to verify it, its
criticality, and its status. The verification column is the one people skimp on
and the one that determines whether the inventory is worth anything.

Verification methods, in descending order of value:

1. An automated test that asserts the behaviour. Best; note the test name.
2. A reproducible manual command: a `curl` with expected status and body shape, a
   CLI invocation with expected output.
3. A documented UI path: navigate here, do this, expect that.
4. Code reading only. Weakest — flag these explicitly, because at the CRC gate
   they are the items you cannot honestly claim to have verified.

Where an item has no automated test and the behaviour is critical, **write the
test now, before consolidating**. A characterisation test written against current
behaviour is the cheapest insurance in the engagement, and it converts a
category-4 item into a category-1 item permanently.

## Coverage, honestly

Count the inventory items by verification method and put those counts in the
report. "142 behaviours inventoried, 96 automated, 31 with reproducible manual
checks, 15 verified by code reading only" is a useful, honest statement. "All
functionality verified" is not, unless every item is in categories 1–3.

The inventory is also the user's guarantee, and it outlives the engagement: it is
the regression checklist for whoever works on this next, and the basis of any
warranty on regressions introduced by the work.
