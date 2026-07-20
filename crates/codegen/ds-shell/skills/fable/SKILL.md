---
name: fable
description: >
  Fable Method toggle and dispatcher (think/act/prove). Fable is always ON in
  the system prompt (harness default). /fable or /fable on re-affirms standing
  instructions; /fable off temporarily deactivates; /fable <task> applies the
  full loop immediately; subcommands: plan, audit, report, loop, judge.
metadata:
  short-description: "Fable Method on/off and subcommands"
  user-invocable: true
---

# The Fable Method

Standing harness mode is **always active** by default (system prompt). This
skill re-affirms, routes sub-commands, or temporarily deactivates.

## Usage

| Invocation | Effect |
|---|---|
| `/fable` or `/fable on` | Re-affirm: Fable active for all subsequent work |
| `/fable off` | Temporary deactivation until re-enabled |
| `/fable <task>` | Full loop on this task immediately |
| `/fable plan <task>` | Steps 0–3 only; no file edits |
| `/fable audit` | Grade recent work against the loop |
| `/fable report` | Rewrite pending answer outcome-first (Step 6) |
| `/fable loop <task>` | Same as `/fable-loop` — full multi-agent orchestration |
| `/fable judge [work]` | Adversarial verdict: VERIFIED / WITH CAVEATS / REFUTED |
| `/fable-loop <task>` | Dedicated orchestration skill (parallel evidence + attackers) |

## The method loop

The full method loop (Steps 0–6: Classify → Define → Gather → Decide → Act → Verify → Report) and orchestration stages (1–4: PLAN → EXECUTE → VERIFY → AUDIT/REPORT) are defined in the system prompt's `<fable_method>` block. This skill only handles toggles and subcommand routing.

## Sub-command routing

If the user typed `/fable off` (or deactivate/stop): acknowledge deactivation;
return to normal judgment without the loop requirement until re-enabled.

If `/fable` / `on` / `activate` with no task: confirm Fable is ACTIVE; wait for work.

If `plan` / `audit` / `report` / `judge` (optionally with args): execute that
mode now under Fable rules.

If `loop` (optionally with a task): run the **orchestrated** four-stage Fable
Loop (see `/fable-loop`): parallel evidence fan-out → main-thread execute →
adversarial verify → outcome-first report.

If `/fable <task>`: apply the full method loop immediately; for multi-area or
consequential work prefer the orchestrated stages (parallel subagents).
