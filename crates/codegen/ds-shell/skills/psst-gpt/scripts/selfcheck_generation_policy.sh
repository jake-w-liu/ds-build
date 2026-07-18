#!/usr/bin/env bash
# Drives --selfcheck-generation-policy (pure state-machine cases, no ChatGPT).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_zip_upload.swift"
OUT="$(swift "$HELPER" --selfcheck-generation-policy 2>/dev/null)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
exit 0
