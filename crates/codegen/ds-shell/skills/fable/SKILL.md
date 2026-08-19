---
name: fable
description: >
  Fable Method toggle and dispatcher (think/act/prove). Fable is OFF by default.
  /fable or /fable on enables it; /fable off turns it back off; /fable <task>
  applies the full loop; subcommands: plan, audit, report, loop, judge.
metadata:
  short-description: "Fable Method on/off and subcommands"
  user-invocable: true
---

# The Fable Method

Fable is **off** until the user invokes this skill. Enabling it applies the
method loop and orchestration details for subsequent work.

## Usage

| Invocation | Effect |
|---|---|
| `/fable` or `/fable on` | Enable: Fable active for all subsequent work |
| `/fable off` | Turn Fable off (the default) |
| `/fable <task>` | Full loop on this task immediately |
| `/fable plan <task>` | Steps 0–3 only; no file edits |
| `/fable audit` | Grade recent work against the loop |
| `/fable report` | Rewrite pending answer outcome-first (Step 6) |
| `/fable loop <task>` | Same as `/fable-loop` — full multi-agent orchestration |
| `/fable judge [work]` | Adversarial verdict: VERIFIED / WITH CAVEATS / REFUTED |
| `/fable-loop <task>` | Dedicated orchestration skill (parallel evidence + attackers) |

## The method loop

Applies only after `/fable` / `/fable on` / `/fable <task>`. Never narrate
stage names in user-facing text.

**Trivial gate:** ≤1 file, ≤10 lines, no new behavior, clear path → do it,
check it, 2-sentence report; skip the rest.

**Otherwise (compact loop):**
1. DEFINE done (observable criterion + how verified); freeze scope.
2. GATHER evidence from primary sources; for bug claims run a decisive test first.
3. ACT: smallest correct change; user > spec > tests > code; no speculative refactors.
4. VERIFY by observation (criterion + nearest tests). Tool-based claims need successful trace evidence.
5. REPORT outcome-first; honest caveats; no method scaffolding.

**Math / physics / quantitative research:** use a foreground `attacker-math` or
direct tool-backed recomputation for acceptance-critical claims. Follow MPR
rules strictly.

Expanded orchestration (PLAN → EXECUTE → VERIFY → AUDIT/REPORT, parallel
subagents) is `/fable-loop`.

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
