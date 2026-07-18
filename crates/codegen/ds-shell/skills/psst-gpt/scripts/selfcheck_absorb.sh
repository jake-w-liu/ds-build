#!/usr/bin/env bash
# Pure absorb/merge selfcheck (no ChatGPT): deep harvest must not exponential-duplicate.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_zip_upload.swift"
OUT="$(swift "$HELPER" --selfcheck-absorb 2>/dev/null)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
exit 0
