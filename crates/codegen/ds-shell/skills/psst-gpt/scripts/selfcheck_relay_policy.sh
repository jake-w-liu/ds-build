#!/usr/bin/env bash
# Pure plain-text relay policy cases; does not open or drive ChatGPT.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="${SCRIPT_DIR}/psst_chat_relay.swift"
OUT="$(/usr/bin/swift "$HELPER" --selfcheck-relay-policy)"
echo "$OUT"
echo "$OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
echo "$OUT" | grep -q 'selfcheck-relay-policy'
if echo "$OUT" | grep -q '"pass" : false\|"pass": false\|"pass":false'; then
  echo "selfcheck_relay_policy: a case failed" >&2
  exit 1
fi

AX_OUT="$(/usr/bin/swift "$SCRIPT_DIR/psst_ax_upload.swift" \
  '{"action":"selfcheckCopyPolicy","prompt":"","filePaths":[]}')"
echo "$AX_OUT"
echo "$AX_OUT" | grep -q '"ok" : true\|"ok": true\|"ok":true'
echo "$AX_OUT" | grep -q 'selfcheck-copy-policy'
if echo "$AX_OUT" | grep -q '"pass" : false\|"pass": false\|"pass":false'; then
  echo "selfcheck_relay_policy: direct Copy candidate case failed" >&2
  exit 1
fi

node --input-type=module - "$SCRIPT_DIR/psst_gpt.mjs" <<'NODE'
import { pathToFileURL } from "node:url";

const modulePath = process.argv[2];
const { __testing } = await import(pathToFileURL(modulePath));
const complete = (overrides = {}) => __testing.isAppResponseCompleteSnapshot({
  assistantText: "Complete assistant response.",
  textStableForMs: 60_000,
  isAnswering: false,
  sendReady: false,
  endedObservations: 0,
  captureState: {},
  ...overrides,
});
const checks = [
  ["bare Stop means active", __testing.appResponseControls({ buttonLabels: ["Stop"] }).active],
  ["ambiguous stable text cannot finish", !complete()],
  ["one Send observation cannot finish", !complete({ sendReady: true, endedObservations: 1 })],
  ["two Send observations can finish", complete({ sendReady: true, endedObservations: 2 })],
];
const failed = checks.filter(([, pass]) => !pass);
process.stdout.write(`${JSON.stringify({ status: "node-relay-policy", ok: failed.length === 0, checks })}\n`);
if (failed.length > 0) process.exit(1);
NODE
