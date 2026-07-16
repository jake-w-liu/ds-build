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

## The loop (concise method)

**Triviality gate:** one file, ~≤10 lines, no new behavior → do, check, two-sentence report.

0. **Classify** — question / task / plan-first (plan-first wins ties).
1. **Define done** — observable criterion + verification method.
2. **Gather evidence** — orient first; primary sources; parallelize; establish intent.
3. **Decide** — one recommendation; ask for irreversible outward actions.
4. **Act surgically** — intent gate; smallest correct change; no silent destroy.
5. **Verify by observation** — criterion observed; nearby checks green; ≤3 retry cycles.
6. **Report outcome-first** — no step narration; honest caveats; hostile reread.

Never invent APIs/paths. Authority: user statement > spec > tests > current code.
If code/check/spec disagree, surface that — do not silently make one match another.

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
