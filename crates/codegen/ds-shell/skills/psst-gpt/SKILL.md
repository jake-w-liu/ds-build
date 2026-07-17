---
name: psst-gpt
description: >
  Relay a prompt (or zip + heavy audit) through the ChatGPT macOS desktop app
  via Accessibility, Chat mode only (never Work). Use for /psst-gpt, psst-gpt,
  ChatGPT app relay, zip upload audit. DeepSeek has no vision — shell + AX only.
  Auto-caffeinates on macOS for the helper lifetime.
metadata:
  short-description: "ChatGPT Chat-only relay / zip audit (auto-caffeinate)"
  user-invocable: true
  argument-hint: "[--zip path|--root dir] audit prompt…  |  text prompt"
---

# PsstGPT (DS) — slash-complete workflow

When the user runs **`/psst-gpt …`**, follow this skill end-to-end. Do not invent
ad-hoc shell recipes.

## Hard limits

| Constraint | Reality |
|------------|---------|
| **Screen locked** | **Does not work.** Helpers exit `PSST_GPT_SCREEN_LOCKED`. |
| **Work / Codex usage** | **Never.** Chat only (`Message ChatGPT`). |
| **Wake hold** | On macOS, helpers **auto-start `caffeinate -dims -w <self>`** and stop it on every exit. **Do not** wrap in an extra long-lived `caffeinate` unless the user asks; the script owns lifecycle. |
| **Vision** | None — shell + AX only. |

## Scripts

```text
~/.ds/skills/psst-gpt/scripts/psst_chat_relay.swift   # text
~/.ds/skills/psst-gpt/scripts/psst_zip_upload.swift    # zip/root audit (preferred for full codebase)
```

Also under the same `scripts/` next to this `SKILL.md` after extract.

## Slash routing

| User intent | Command |
|-------------|---------|
| Plain text | `swift …/psst_chat_relay.swift --timeout 0 -- "…"` |
| **Full codebase / zip audit** | `swift …/psst_zip_upload.swift --root "$PWD" --timeout 0 -- "…"` |
| Existing zip | `swift …/psst_zip_upload.swift --zip PATH --timeout 0 -- "…"` |
| Same chat follow-up | add `--no-new-chat` |

`--timeout 0` = wait until ChatGPT reply stabilizes (heavy audits).

### Full-codebase audit (default for “zip this full codebase…”)

```bash
SCRIPT="$HOME/.ds/skills/psst-gpt/scripts/psst_zip_upload.swift"
# Prefer skill-dir script if present:
SKILL_DIR="$(dirname "$(find …)")"  # use path of this SKILL.md's scripts/

swift "$HOME/.ds/skills/psst-gpt/scripts/psst_zip_upload.swift" \
  --root "$PWD" --timeout 0 -- \
  "Audit-only instructions for ChatGPT…"
```

Use a **long** `run_terminal_command` timeout (or no cap) so DS does not kill the helper mid-wait.

**Do not make code edits** when the user asked for audit-only — report ChatGPT’s findings.

## Orchestration

1. **Doctor (optional but preferred):**  
   `swift …/psst_chat_relay.swift --doctor`  
   Expect `ok`, `mode: chat`, `workOn: false`, `screenLocked: false`.

2. **Run zip or text helper** as above (auto-caffeinate inside).

3. **Read results** (helpers also stage under the project):
   - stdout JSON: `finalDeliveryText`, `attached`, `workOn`, `responsePath`, `resultPath`
   - `.ds/psst-gpt/last-response.md` — full body
   - `.ds/psst-gpt/last-result.json` — full JSON

4. **Handoff:** Use the full response for “report back”. If `mustReturnVerbatim`, preserve ChatGPT’s body. For “reverify”, re-read the staged files and confirm they are a real audit (not chrome / zip filename only).

## Failure codes

| Code | Action |
|------|--------|
| `PSST_GPT_SCREEN_LOCKED` | Unlock Mac; retry. |
| `WORK_MODE` | Switch to Chat. |
| `ATTACHMENT_MISSING` | Retry zip attach. |
| `TIMEOUT` | Use `--timeout 0`; ensure ChatGPT still answering. |

## Setup

- Unlocked macOS console, ChatGPT running on **Chat**, signed in  
- Accessibility for host / swift  
- Bundle id may be `com.openai.codex`

## CRITICAL

Never use Work. Never invent an audit if the helper failed.  
When complete with non-empty `finalDeliveryText`, that text is the audit deliverable for DS continuation.
