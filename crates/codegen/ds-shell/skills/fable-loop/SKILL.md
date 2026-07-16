---
name: fable-loop
description: >
  End-to-end orchestrated Fable workflow: parallel evidence subagents, one
  committed plan, surgical main-thread execution, adversarial verification
  agents, honest outcome-first report. Use when the user invokes /fable-loop,
  /fable loop, says "run the fable loop", "orchestrate this", or wants full
  multi-agent Fable-style execution of a non-trivial multi-step task.
metadata:
  short-description: "Orchestrated Fable: plan → execute → attack → report"
  user-invocable: true
---

# The Fable Loop (orchestration)

This skill orchestrates the Fable method: the method says **WHAT** to check;
this loop says **WHO** does the work — main thread vs parallel subagents vs
adversarial attackers.

**Gate first.** Trivial (one file, ~≤10 lines, no new behavior, path clear): do
it, one obvious check, two-sentence report. No stages, no subagents.
Everything else runs the four stages **in order**.

## Resource bounds (hard)

- Prefer `explore` for research; `general-purpose` only when edits are required.
- Max **4** evidence subagents per batch; max **3** attackers; max **8** live at once.
- One evidence batch + one follow-up; a third needs a stated reason.
- Subagents return **distilled** findings with `file:line` cites — no raw dumps.
- Cancel workers you no longer need; do not design deep nested spawn trees.
- Solo when single-area or a direct tool call is faster than an agent.

## Stage 1 — PLAN

1. Method Steps 0–3: classify, define done + named verification, state assumptions.
2. **Evidence fan-out** — spawn gatherers as **parallel** subagents in **one**
   message (never sequential when independent), within bounds above:
   - codebase: `explore` per distinct area;
   - libraries/facts: research / web_search-oriented work;
   - distilled findings only.
3. **Plan artifact:** classification; done + verification; evidence (cited);
   ONE approach (alternatives dismissed in one line each); risks/assumptions;
   execution checklist.
4. **Decision gate:** task-shaped + reversible → Stage 2 without asking.
   Plan-first / irreversible / outward-facing → present plan and STOP for approval.

## Stage 2 — EXECUTE

1. Work the checklist in the **main** thread (task tools if available). Deciding
   and editing stay main-thread; only search/verify fan out.
2. Every edit: method Step 4 (intent gate, smallest change, never destroy blind).
3. Independent mechanical multi-file work may fan out in one message with
   isolation if files could collide.
4. Surprises re-route: say them, update plan or return to Stage 1.

## Stage 3 — VERIFY (adversarial)

1. Run named verification yourself: done criterion **observed**; surrounding
   build/tests healthy.
2. **Consequential changes:** spawn **1–3 parallel attacker** subagents, each
   a distinct lens, e.g.:
   - "Read this diff and prove it is wrong or incomplete"
   - "Exercise the changed behavior and find a breaking input"
   - "Check claims against spec/docs and find a contradiction"
3. Surviving findings → Stage 2. Hard bound: 3 failed fix-verify cycles on the
   same issue, or blockers outside control → stop and hand back with hypothesis.

## Stage 4 — AUDIT and REPORT

1. Self-audit method steps: followed / skipped / faked; fix what one pass can.
2. Outcome-first report (method Step 6). **No stage names or step numbers** in
   user-facing text. Honest caveats. Follow-ups only if they emerged from the work.

## When NOT to use

- Trivial gate cases.
- Pure questions with no multi-step work (plain Fable method is enough).
- Nested inside another orchestrated goal/plan phase — apply method rules
  inside that phase instead of nesting loops.

## Model economy

Evidence and attacker subagents may use a cheaper/faster model when the
platform allows; keep decide/edit on the strongest model. Prefer higher
reasoning effort on attackers than on gatherers when you can choose.

## Task

If the user provided a task after the skill name, apply this loop to it **now**.
If none, confirm orchestration mode is ready and wait for the next task.
