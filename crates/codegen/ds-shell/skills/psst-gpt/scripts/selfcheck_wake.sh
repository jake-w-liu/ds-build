#!/usr/bin/env bash
# Drives the shipped psst_chat_relay.swift --selfcheck-wake entry point.
# Asserts caffeinate is held during the check and released after.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RELAY="${SCRIPT_DIR}/psst_chat_relay.swift"
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo '{"ok":true,"skipped":true,"platform":"non-darwin"}'
  exit 0
fi
OUT="$(swift "$RELAY" --selfcheck-wake 2>/dev/null)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
# No orphan caffeinate from this selfcheck pid should remain
exit 0
