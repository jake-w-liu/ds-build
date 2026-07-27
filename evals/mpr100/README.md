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

Prepare a run from the original corpus:

```sh
workspace="$(
  python3 evals/mpr100/prepare_clean_run.py \
    /Users/jake/Downloads/mpr100_agent_test
)"
```

Run `ds` with that printed path as its working directory. Administer the same
two inputs on a new session:

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
