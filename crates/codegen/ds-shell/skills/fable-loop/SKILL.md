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

## How it works

The orchestration stages (1–4: PLAN → EXECUTE → VERIFY → AUDIT/REPORT), resource bounds (max subagents, model preferences, file:line citation rules), and attacker spawning directives are defined in the system prompt's `<fable_method>` block. This skill activates them for the current task.

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
