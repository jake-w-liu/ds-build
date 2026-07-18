#!/usr/bin/env bash
# Drives the shipped psst_zip_upload.swift --selfcheck-finish-rules entry point.
# Asserts incomplete loading chrome / fragment salad / short stubs are rejected,
# and a substantive multi-section audit body is accepted.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ZIP="${SCRIPT_DIR}/psst_zip_upload.swift"
OUT="$(/usr/bin/swift "$ZIP" --selfcheck-finish-rules)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
echo "$OUT" | grep -q 'selfcheck-finish-rules'
# Each fixture case should report pass:true
if echo "$OUT" | grep -q '"pass" : false\|"pass": false\|"pass":false'; then
  echo "selfcheck_finish_rules: a case failed" >&2
  exit 1
fi
exit 0
