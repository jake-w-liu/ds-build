#!/usr/bin/env bash
# Drives psst_zip_upload.swift --selfcheck-longrun-policy.
# Asserts short timeout auto-upgrade + lock-park / ax-refresh markers in shipped helper.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_zip_upload.swift"
OUT="$(/usr/bin/swift "$HELPER" --selfcheck-longrun-policy)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
echo "$OUT" | grep -q 'selfcheck-longrun-policy'
if echo "$OUT" | grep -q '"pass" : false\|"pass": false\|"pass":false'; then
  echo "selfcheck_longrun_policy: a case or marker failed" >&2
  exit 1
fi
exit 0
