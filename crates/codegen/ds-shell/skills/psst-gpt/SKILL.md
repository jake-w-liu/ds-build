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
| **Screen locked** | AX cannot drive UI while locked. Helpers **park** (`waiting-screen-unlock` / `PSST_GPT_SCREEN_LOCKED_PARKED`), keep caffeinate, and **resume after unlock** when `--timeout 0`. They only hard-fail `PSST_GPT_SCREEN_LOCKED` if a positive `--timeout` deadline expires still locked. Manual unlock is still required (caffeinate cannot unlock). |
| **Work / Codex usage** | **Never.** Chat only (`Message ChatGPT`). |
| **Wake hold** | On macOS, helpers use a **multi-layer** hold for long audits (host `displaysleep` can be ~2m): (1) primary `caffeinate -dims -w <self>`, (2) periodic `caffeinate -u -t 120` user-active pulses, (3) `ensureAlive()` restarts a dead primary from the wait loop. Released on every exit. Only while the **Swift helper** runs — **do not** wrap in an extra long-lived `caffeinate` unless the user asks. |
| **Host / DS bash timeout** | Production `ds` allows up to **10h** FG (`max_timeout_secs=36000`) and auto-backgrounds long commands. When invoking the helper via bash, set **`timeout: 36000000`** (ms) **or** `timeout: 0` with `background: true` then wait on the task until exit. **Never** use 30s/120s for zip/Pro audits. |
| **Short `--timeout N`** | N>0 and N&lt;3600 **auto-upgrades to unlimited (0)** unless `--timeout-strict` (prevents accidental cutoffs). Prefer `--timeout 0`. |
| **AX flake (long runs)** | Wait loop re-activates ChatGPT + refreshes AX root periodically; Copy-message harvest retries with a slow second pass. Still depends on Accessibility remaining granted. |
| **Vision** | None — shell + AX only. |

## Scripts

Prefer **scripts next to this `SKILL.md`** (project `.ds/skills/psst-gpt/scripts/` when the repo skill is installed, else `~/.ds/skills/psst-gpt/scripts/`). Keep them byte-identical to `crates/codegen/ds-shell/skills/psst-gpt/scripts/` — re-sync after installs if `run_full_codebase_audit.sh` exits `STALE_HELPER`.

```text
scripts/psst_chat_relay.swift          # text
scripts/psst_zip_upload.swift           # zip/root audit
scripts/run_full_codebase_audit.sh      # one-shot full-tree entry
```

## Slash routing

| User intent | Command |
|-------------|---------|
| Plain text | `swift …/psst_chat_relay.swift --timeout 0 -- "…"` |
| **Full codebase / zip audit** | `swift …/psst_zip_upload.swift --root "$PWD" --timeout 0 -- "…"` |
| Existing zip | `swift …/psst_zip_upload.swift --zip PATH --timeout 0 -- "…"` |
| Same chat follow-up | add `--no-new-chat` |

`--timeout 0` = **wait until generation ends** (no wall-clock cap — Pro thinking
can take **hours**). Short positive caps (e.g. 30/120/300) auto-upgrade to `0`
unless `--timeout-strict`. Finish detection is a **signal-driven state machine**, not
timers:

| Phase | Meaning | Exit? |
|-------|---------|-------|
| `awaitingStart` | After send; no Stop/growth for this turn yet | No |
| `active` | Stop without Send, or loading chrome (thinking/streaming) | **Never** (hours OK) |
| `settling` | UI shows ended (Send ready / Stop gone); deep harvest + fingerprint stability | No until settled |
| `complete` | Acceptable body + stable harvests | Yes → stage full reply |
| `captureFailed` | Generation ended but body never passed complete checks | Yes → honest partial |

Never treats “no body growth” as done while **active**. After end: **Copy message**
harvest so full audits reach `.ds/psst-gpt/last-response.md`.
`--timeout N` (N>0) is an optional user wall-clock cap only.

Selfcheck: `bash …/selfcheck_generation_policy.sh` (pure phase cases, no ChatGPT).

### Full-codebase audit (default for “zip this full codebase…”)

**One shot. Foreground helper. `--timeout 0` only.** Do **not** use 30s helper timeouts, do **not** fall back to text-only when the user asked for a zip attach.

```bash
# Resolve scripts next to this skill (project first, then user install):
SKILL_SCRIPTS=".ds/skills/psst-gpt/scripts"
[[ -x "$SKILL_SCRIPTS/run_full_codebase_audit.sh" ]] || SKILL_SCRIPTS="$HOME/.ds/skills/psst-gpt/scripts"

# Preferred entry (blocks until ChatGPT stabilizes; stages .ds/psst-gpt/*):
# DS bash tool: set timeout: 36000000  (10 hours, ms) — REQUIRED for long Pro thinking.
bash "$SKILL_SCRIPTS/run_full_codebase_audit.sh" "$PWD"

# Equivalent (GPT-facing prompt only — no operator meta like "Chat only / never Work"):
swift "$SKILL_SCRIPTS/psst_zip_upload.swift" \
  --root "$PWD" --timeout 0 -- \
  "Attached is source-archive.zip of a full Rust monorepo…. Produce a structured audit: (1) top risks by severity, (2) architecture notes, (3) concrete recommendations. Do not edit code."
```

**Prompt split (important):**
- **For DS / this skill:** stay in Chat, never Work, audit-only (no code edits), wait for full capture.
- **For ChatGPT (the string after `--`):** only the audit ask about the zip — do **not** paste operator meta (“Chat only — never Work”, “AUDIT ONLY for the assistant”, etc.).

**Host tool timeout (critical):** when DS runs the command above via bash, pass **`timeout: 36000000`** (milliseconds = 10 hours). Production `ds` FG ceiling is 10h and may auto-background long commands — still set the long timeout so the wrapper does not kill the helper early. Never 30s/120s.

If ChatGPT returns a **Work-mode nudge / “Continue with Work?” / cannot open zip in Chat** body with `ok: true` and non-empty `finalDeliveryText`, that **is** a complete result — **stop**, report it, do not invent more retries or text-only substitutes.

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
| `PSST_GPT_SCREEN_LOCKED_PARKED` / `waiting-screen-unlock` | Not a failure — unlock Mac; helper resumes. |
| `PSST_GPT_SCREEN_LOCKED` | Stayed locked until deadline; unlock and re-run with `--timeout 0`. |
| `WORK_MODE` | Switch to Chat. |
| `ATTACHMENT_MISSING` | Retry zip attach. |
| `TIMEOUT` | Use `--timeout 0` (and host bash `timeout: 36000000`); ensure ChatGPT still answering. |

## Setup

- Unlocked macOS console, ChatGPT running on **Chat**, signed in  
- Accessibility for host / swift  
- Bundle id may be `com.openai.codex`

## CRITICAL

Never use Work. Never invent an audit if the helper failed.  
When complete with non-empty `finalDeliveryText`, that text is the audit deliverable for DS continuation.

For **zip / full codebase**: run `run_full_codebase_audit.sh` (or zip helper with `--timeout 0`) **once**, wait for exit, read staged JSON/md.  
**Forbidden:** short timeouts, background+poll loops, abandoning zip for a text-only “describe the tree” substitute when the user asked to attach the zip.
