#!/usr/bin/env bash
# One-shot full-tree zip audit via the ChatGPT macOS Chat composer.
# Operator/DS constraints (Chat only, never Work, audit-only orchestration) are
# enforced by this skill + helper — do NOT put them in the GPT-facing prompt.
set -euo pipefail
ROOT="${1:-$PWD}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_zip_upload.swift"
# Default message is for ChatGPT only (what it should produce from the zip).
PROMPT="${2:-Attached is source-archive.zip of a full Rust monorepo (excludes target/.git/node_modules). Produce a structured audit: (1) top risks by severity, (2) architecture notes, (3) concrete recommendations. Do not edit code. If you cannot open the zip, say so clearly and give any overview you can from the attachment.}"

if [[ ! -f "$HELPER" ]]; then
  echo '{"ok":false,"code":"HELPER_MISSING"}' >&2
  exit 2
fi
# Refuse stale extracts missing this transport revision or required policies.
if ! grep -q '^// PSST_TRANSPORT_REV=5$' "$HELPER" \
  || ! grep -q 'double-check' "$HELPER" \
  || ! grep -q 'not treating as sent' "$HELPER" \
  || ! grep -q 'avg < 48' "$HELPER" \
  || ! grep -q 'generation-state-machine' "$HELPER" \
  || ! grep -q 'classifyGenerationPhase' "$HELPER" \
  || ! grep -q 'ComposerControls' "$HELPER" \
  || ! grep -q 'mergeReplyBody' "$HELPER" \
  || ! grep -q 'merge=non-dup' "$HELPER" \
  || ! grep -q 'PSST_GPT_SCREEN_LOCKED_PARKED' "$HELPER" \
  || ! grep -q 'resolveHelperTimeoutSec' "$HELPER" \
  || ! grep -q 'refreshAxRoot' "$HELPER" \
  || ! grep -q 'waitWhileScreenLocked' "$HELPER"; then
  echo '{"ok":false,"code":"STALE_HELPER","message":"psst_zip_upload.swift missing send-verify/generation-state-machine/long-run-park markers; re-sync skill from crates/codegen/ds-shell/skills/psst-gpt"}' >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo '{"ok":false,"code":"NOT_DARWIN"}' >&2
  exit 2
fi

cd "$ROOT"
# The bash tool may auto-background this one helper. If it returns a task_id,
# keep the same DS turn alive and wait on that task until exit; do not relaunch.
# The helper's own --timeout 0 has no response wall-clock deadline.
set +e
RUN_OUTPUT="$(mktemp -t psst-gpt-wrapper.XXXXXX)"
cleanup() {
  rm -f -- "$RUN_OUTPUT"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
/usr/bin/swift "$HELPER" --root "$ROOT" --timeout 0 -- "$PROMPT" | tee "$RUN_OUTPUT"
EC=${PIPESTATUS[0]}
set -e
STAGE_ID="$(sed -n 's/.*"handoffStageId"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$RUN_OUTPUT" | tail -n 1)"
if [[ -n "$STAGE_ID" ]] \
  && [[ -f .ds/psst-gpt/last-result.json ]] \
  && grep -Fq "\"stageId\" : \"$STAGE_ID\"" .ds/psst-gpt/last-result.json; then
  echo "staged_result=.ds/psst-gpt/last-result.json" >&2
  if [[ -f .ds/psst-gpt/last-response.md ]]; then
    echo "staged_response_bytes=$(wc -c < .ds/psst-gpt/last-response.md | tr -d ' ')" >&2
  fi
else
  echo "staged_result=unavailable-current-run" >&2
fi
exit "$EC"
