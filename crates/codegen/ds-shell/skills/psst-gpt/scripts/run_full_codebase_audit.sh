#!/usr/bin/env bash
# One-shot full-tree zip audit via ChatGPT Chat (never Work).
set -euo pipefail
ROOT="${1:-$PWD}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_zip_upload.swift"
PROMPT="${2:-AUDIT ONLY. Chat only — never Work. Attached is source-archive.zip of a full Rust monorepo (excludes target/.git/node_modules). Read the zip and produce a structured audit: (1) top risks by severity, (2) architecture notes, (3) concrete recommendations. Do not edit code. If you cannot open the zip in Chat, say so clearly (including any Work-mode nudge). Reply in Chat with the full text.}"

if [[ ! -f "$HELPER" ]]; then
  echo '{"ok":false,"code":"HELPER_MISSING"}' >&2
  exit 2
fi
# Refuse stale extracts missing send-verify / incomplete-body guards
if ! grep -q 'double-check' "$HELPER" || ! grep -q 'not treating as sent' "$HELPER" || ! grep -q 'avg < 48' "$HELPER"; then
  echo '{"ok":false,"code":"STALE_HELPER","message":"psst_zip_upload.swift missing send-verify/finish-rule markers; re-sync skill from crates/codegen/ds-shell/skills/psst-gpt"}' >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo '{"ok":false,"code":"NOT_DARWIN"}' >&2
  exit 2
fi

cd "$ROOT"
set +e
/usr/bin/swift "$HELPER" --root "$ROOT" --timeout 0 -- "$PROMPT"
EC=$?
set -e
if [[ -f .ds/psst-gpt/last-result.json ]]; then
  echo "staged_result=.ds/psst-gpt/last-result.json" >&2
fi
if [[ -f .ds/psst-gpt/last-response.md ]]; then
  echo "staged_response_bytes=$(wc -c < .ds/psst-gpt/last-response.md | tr -d ' ')" >&2
fi
exit "$EC"
