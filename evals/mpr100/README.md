# MPR-100 development calibration

This directory is the control plane for the public 20-item MPR-100 development
set. The benchmark source remains in a separate directory and the model must
never receive the reference answer or grader.

## Scope and score

The supplied development corpus contains 20 items. The deterministic rubric
awards five points per item:

- one point for strict submission-format compliance;
- four points for four independently named mathematical evidence criteria.

The resulting total is on a 100-point scale. It is a deterministic evidence
coverage score, not a theorem prover: a high score must still be compared with
the verified reference before making a correctness claim. Conversely, an
equivalent notation can produce a false negative; the grader prints the exact
criterion that was not recognized so an auditor can decide whether the
submission or the notation recognizer is at fault.

`score_submission.py` is a task-specific development diagnostic, not an
official semantic MPR grader or a product completion contract. Do not change
general-purpose prompts or runtime gates merely to satisfy its lexical
recognizer; judge equivalent mathematics against the task and verified
reference.

## Files

- `mpr100_answer_sheet_verified.tex`: completed auditable reference answers.
- `verify_reference.py`: independent symbolic, numerical, recurrence,
  brute-force, and dimensional checks for all 20 reference items.
- `score_submission.py`: deterministic structure and evidence grader.
- `prepare_clean_run.py`: hash-pins the four source files and prepares an
  isolated model workspace containing only those files.

## Verification

From the repository root:

```sh
python3 evals/mpr100/verify_reference.py
python3 evals/mpr100/score_submission.py \
  evals/mpr100/mpr100_answer_sheet_verified.tex \
  --require-at-least 100
```

If a TeX engine is installed, also compile the reference:

```sh
mkdir -p /tmp/mpr100-reference-tex
pdflatex -halt-on-error -interaction=nonstopmode \
  -output-directory=/tmp/mpr100-reference-tex \
  evals/mpr100/mpr100_answer_sheet_verified.tex
```

## Clean evaluation loop

`/goal /structure` does NOT select a specialized agent profile — agent
selection is explicit (`--agent-profile`, `[agent]`, `DS_AGENT`, or the
default `ds-build`). The MPR profile ships as
`crates/codegen/ds-agent/examples/agents/mpr-researcher.md` and must be passed
explicitly, or installed to `~/.ds/agents/mpr-researcher.md`.

Prepare a run from the original corpus:

```sh
workspace="$(
  python3 evals/mpr100/prepare_clean_run.py \
    /Users/jake/Downloads/mpr100_agent_test \
    --agent-profile /path/to/mpr-researcher.md
)"
```

The prepared run directory contains:

- `workspace/` — exactly the four hash-pinned benchmark inputs;
- `run.sh` — the ONLY supported launch path: it runs `ds` with
  `--sandbox strict --no-memory --disable-web-search --agent-profile <profile>`
  and `DS_SANDBOX_FAIL_CLOSED=1` (the run REFUSES to start if the strict
  sandbox cannot be applied — unsandboxed runs are never silent);
- `run_manifest.json` — runtime-observed metadata: ds binary version + baked
  commit, profile path + hash, administered model, launch argv, sandbox /
  memory / web-search policy, corpus hashes, and the final artifact SHA-256
  (written when the run finishes).

Run the launcher (a bare `ds` invocation skips the isolation flags and the
manifest finalization):

```sh
./"$workspace"/../run.sh
```

Administer the same two inputs on a new session:

```text
/headroom on
/goal /structure start from AGENT_INSTRUCTIONS.txt and complete all.
```

Do not copy any file from this control directory into the model workspace.
After the run, score the completed sheet:

```sh
python3 evals/mpr100/score_submission.py \
  "$workspace/mpr100_answer_sheet_development.tex"
```

For every miss, inspect the transcript and identify the first unsupported or
incorrect step before changing the harness. A harness change is justified only
by a reproduced behavior and must be followed by a fresh clean workspace.

## Completion gate and validator

The `mpr-researcher` profile declares a `completionRequirement` on
`mpr_validate_artifact`, a deterministic validator registered in the ds-build
toolset (NOT `score_submission.py` — it contains no benchmark answers and
cannot be gamed toward lexical targets). It parses the artifact's
`%<MPR:BEGIN id=…>` blocks and requires, per item: the six solution fields
(assumptions / derivation / final `\boxed` / independent checks / tools &
evidence / confidence), no placeholders or abstentions, balanced LaTeX
environments, and — with `require_evidence_manifest=true` — a matching
successful record in `evidence_manifest.json` for every tool-confirmation
claim. The gate treats the validator's TOOL ERROR as failure and retries the
turn; the goal cannot complete until the exact artifact passes.

## Benchmark hygiene knobs (recommended for a private-form run)

- `[goal] fail_closed_verification = true` (env `DS_GOAL_FAIL_CLOSED_VERIFICATION=1`):
  verification INFRASTRUCTURE failures pause the goal instead of recording
  `Achieved`.
- `[goal] strict_skeptic_verdicts = true` (env `DS_GOAL_STRICT_SKEPTIC_VERDICTS=1`):
  a skeptic whose verdict JSON is missing/malformed votes a synthetic REFUTE;
  an unstructured terminal token can never approve.
- `[goal] skeptic_models = [...]`: use at least one skeptic model
  heterogeneous with the solver to avoid shared-model blind spots.
- Freeze the harness and the profile hash (recorded in the manifest) before
  touching an unopened private form.
