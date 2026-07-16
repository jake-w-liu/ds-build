---
name: deep-debug
description: >
  Fresh, complete, iterative deep debugging workflow for finding, verifying,
  fixing, and rechecking real software bugs across domain correctness,
  implementation correctness, memory/resource behavior, and performance where
  applicable. Use when the user asks for deep-debug, deep debugging, a fresh
  bug audit, a bug hunt, an audit, verify, fix, and reverify loop, or wants the
  assistant to keep investigating until no confirmed bugs remain while avoiding
  false positives.
metadata:
  short-description: "Deep audit → verify → fix → CRC reverify"
  user-invocable: true
---

# Deep Debug

## Scope and Standard

Default mode is a fresh, complete audit. Treat `deep-debug`, `deep debugging`, `fresh audit`, `bug hunt`, `audit`, or a verify/fix/reverify loop as requiring a new end-to-end pass over the applicable target surface. Do not narrow to a previous bug, recent diff, one suspected file, the failing test, or a remembered hypothesis unless the user explicitly gives that bounded scope.

A fresh audit means rebuilding the repo and problem map from current sources, commands, tests, logs, and docs in this run. Conversation history and earlier findings are context only; they do not count as verification, coverage, or a stop condition. Re-open relevant files, re-run current checks where feasible, and re-derive the candidate list before making any verdict.

A deep audit means covering the full applicable surface: public entry points, core internal flows, data/model invariants, domain formulas, state transitions, boundary/error paths, concurrency/resource behavior, configuration/CI, tests, and dependency or platform boundaries. If the repository is too large to finish in one pass, state the partial coverage and continue iterating; do not silently downgrade to a narrow audit.

Verification is mandatory for every finding and code claim. Apply CRC to every fix and every post-fix verdict:

- Correctness: logic, boundaries, null/empty/error paths, units, invariants, numerical stability, and algorithm validity.
- Robustness: realistic input range, configuration variance, concurrency, partial failure, cleanup, and resource pressure.
- Completeness: production-ready behavior end to end, with no fake outputs, swallowed errors, or placeholder handling.

Do not claim code is "fully optimized", "memory efficient", "physically correct", or "mathematically correct" without evidence. If full proof is not achievable in the current environment, say exactly what was verified, what remains unverified, and what test, benchmark, profile, or reference would close the gap.

## Workflow

Run a fresh iterative bug hunt:

1. Rebuild context from the current repo/worktree, user request, failing behavior, logs, tests, CI, docs/contracts, package or app entry points, and recent diffs. Do not rely on prior assumptions when starting a new pass.
2. Establish the audit matrix for the target and record the actual surface you intend to cover. Unless clearly irrelevant after inspecting enough context, include these dimensions:
   - domain/science correctness: formulas, units, invariants, physical constraints, numerical methods, tolerances, and known-good reference behavior.
   - implementation correctness: control flow, data flow, state transitions, API contracts, persistence, configuration, and error paths.
   - memory/resource behavior: allocations, leaks, ownership/lifetime, buffer growth, cleanup, and non-memory resources such as file, socket, GPU, or handle usage.
   - performance/optimization: asymptotic cost, measured hotspots, avoidable copies or allocations, synchronization overhead, and whether the implementation meets explicit targets.
3. Turn each candidate into a falsifiable claim with expected behavior, actual behavior, affected code path, and evidence.
4. Verify before fixing. Prefer a failing test, reproduction command, runtime trace, typecheck/build failure, direct control-flow/data-flow proof, profiler capture, allocation telemetry, benchmark delta, invariant check, worked example, or comparison against trusted reference output. Discard candidates that cannot survive verification.
5. Fix only confirmed bugs. Keep changes local, follow existing patterns, preserve unrelated user changes, and add or update regression tests when feasible.
6. Reverify each fix under CRC:
   - Correctness: rerun the original failure, new/updated tests, and nearby logic checks; cover boundaries and domain invariants that could regress.
   - Robustness: run variant, stress, or error-path checks proportional to risk; confirm cleanup and realistic-input behavior.
   - Completeness: verify the full intended path works end to end without placeholders, silent fallbacks, or partially fixed behavior.
7. Repeat from a fresh audit pass until no new verified bugs are found. Each repeat must start from the audit matrix and current code, not only from the previous fix or previous candidate.

## Narrowing Rules

Run a narrow audit only when the user explicitly asks for a bounded scope, such as a specific file, function, module, failing behavior, or command. Even then:

- State the boundary before investigating.
- Run a fresh, deep audit inside that boundary.
- Do not imply the whole repo or broader system was audited.
- If the user asks for a fresh audit after a prior pass or challenges that the audit was narrow, reset to the default complete audit.

## Goal Handling

When goal tools are available, create a goal before starting only if the user explicitly asks to set the deep-debug run as a goal. Treat prompts like `Use $deep-debug and set this as a goal...` or `Use deep-debug as a goal...` as explicit goal requests.

Do not create a goal from `$deep-debug` alone. Goal creation requires an explicit user request.

If a goal is created, use an objective that names the target and includes the full loop plus any mandatory dimensions in scope: audit from scratch, verify, fix, and CRC-reverify until no confirmed bugs remain. Mark the goal complete only after the stop condition is satisfied.

## False-Positive Guardrails

- Do not present style issues, speculative improvements, missing features, or harmless edge cases as bugs unless they violate an explicit requirement or observable behavior.
- Separate confirmed bugs from unverified risks and test gaps. Fix confirmed bugs; report unverified risks only when they matter and cannot be checked within the available constraints.
- Check whether suspicious code is intentional compatibility behavior, generated output, test-only scaffolding, feature-flagged behavior, platform-specific behavior, or covered by callers before calling it a bug.
- If verification needs current external facts, installed package behavior, or service documentation, look it up or reproduce locally instead of guessing.
- Treat memory/performance work the same way: do not label code inefficient or unoptimized without measurement, and do not declare optimization complete without an explicit target or a clearly bounded audit scope.

## Fresh-Pass Tactics

- On each iteration, change the audit angle instead of only re-reading the previous finding: follow a different entry point, inspect neighboring modules, run a different test scope, or trace a different data shape.
- For the final pass, choose at least one independent route that was not the path to the last fix: public API index, test suite map, config/CI path, dependency boundary, domain invariant list, or resource-lifetime path.
- Search broadly before concluding: enumerate files or modules, inspect representative entry points and callers, and connect tests to implementation paths rather than relying on the files already touched.
- When multi-agent tools are available, current instructions permit delegation, and the task is broad enough, launch an independent fresh audit with only the task-relevant context and compare concrete evidence, not conclusions.
- Keep a compact ledger of candidates: `candidate`, `dimension`, `verification`, `status`, `fix`, and `reverify`.

## Stop Condition

Stop only after:

- the audit surface was rebuilt from current sources, commands, tests, logs, and docs in this run,
- all confirmed bugs found so far are fixed or explicitly blocked,
- each fix has been reverified under CRC,
- each relevant audit dimension was either verified or explicitly marked not applicable with a reason,
- any claims about math/physics correctness, memory efficiency, or optimization are backed by tests, proofs, profiles, or benchmarks,
- one final fresh audit pass, using an independent route through the code rather than only regression-checking the fixes, finds no new verified bugs, and
- the final response states what was fixed, what was run, and any remaining unverified risks or test gaps.
