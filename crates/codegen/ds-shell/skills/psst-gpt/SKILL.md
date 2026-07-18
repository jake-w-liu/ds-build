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
| **Host / DS bash lifecycle** | Production `ds` may auto-background a foreground bash call after its short turn-blocking budget. That is a transport transition, **not completion**: keep the same `task_id`, wait for completion/auto-wake, and re-read task output as often as needed with **no overall polling cap**. Background helpers have no wall-clock deadline. Never launch a second helper, use `nohup`/manual `&`, or return while the task is pending. |
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

**One helper process. `--timeout 0` only.** The bash tool may return a
`task_id` after auto-backgrounding; keep waiting on that exact task until it
exits. Do **not** use short helper timeouts, do **not** start a replacement
helper, and do **not** fall back to text-only when the user asked for a zip.

```bash
# Resolve scripts next to this skill (project first, then user install):
SKILL_SCRIPTS=".ds/skills/psst-gpt/scripts"
[[ -x "$SKILL_SCRIPTS/run_full_codebase_audit.sh" ]] || SKILL_SCRIPTS="$HOME/.ds/skills/psst-gpt/scripts"

# Preferred entry (stages .ds/psst-gpt/* when the helper exits):
bash "$SKILL_SCRIPTS/run_full_codebase_audit.sh" "$PWD"

# Equivalent (GPT-facing prompt only — no operator meta like "Chat only / never Work"):
swift "$SKILL_SCRIPTS/psst_zip_upload.swift" \
  --root "$PWD" --timeout 0 -- \
  "Attached is source-archive.zip of a full Rust monorepo…. Produce a structured audit: (1) top risks by severity, (2) architecture notes, (3) concrete recommendations. Do not edit code."
```

**Prompt split (important):**
- **For DS / this skill:** stay in Chat, never Work, audit-only (no code edits), wait for full capture.
- **For ChatGPT (the string after `--`):** only the audit ask about the zip — do **not** paste operator meta (“Chat only — never Work”, “AUDIT ONLY for the assistant”, etc.).

**Host task handoff (critical):** if bash returns `signal: auto_backgrounded`
or a `task_id`, the helper is still running. Wait on that task repeatedly (a
bounded wait call is only a check-in, never an overall response deadline) or
consume its completion auto-wake. Continue until the task reports completion,
then read its full output plus the staged JSON/Markdown. Never interpret
`pending`, an empty poll, or unchanged partial text as the answer.

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

   A relay is successful **only** when its current-run JSON says both `ok: true`
   and `status: "complete"`. If the helper exits nonzero, says `ok: false`, or
   says `status: "partial"`, report the exact failure code and treat any `partial`
   body as diagnostic evidence only. Never present, summarize, repair, or add
   missing text to a partial body as though it were ChatGPT’s complete response.
   Never synthesize requested markers or other content that ChatGPT omitted.

## Failure codes

| Code | Action |
|------|--------|
| `PSST_GPT_SCREEN_LOCKED_PARKED` / `waiting-screen-unlock` | Not a failure — unlock Mac; helper resumes. |
| `PSST_GPT_SCREEN_LOCKED` | Stayed locked until deadline; unlock and re-run with `--timeout 0`. |
| `WORK_MODE` | Switch to Chat. |
| `ATTACHMENT_MISSING` | Retry zip attach. |
| `TIMEOUT` | Use helper `--timeout 0`; if bash auto-backgrounded, keep waiting on the same task until exit. |

## Setup

- Unlocked macOS console, ChatGPT running on **Chat**, signed in  
- Accessibility for host / swift  
- Bundle id may be `com.openai.codex`

## CRITICAL

Never use Work. Never invent an audit if the helper failed.  
When complete with non-empty `finalDeliveryText`, that text is the audit deliverable for DS continuation.

For **zip / full codebase**: run `run_full_codebase_audit.sh` (or zip helper with `--timeout 0`) **once**, follow the same process/task until exit, then read staged JSON/md.
**Forbidden:** short response deadlines, launching duplicate/manual-background helpers, returning while the auto-background task is pending, or abandoning zip for a text-only “describe the tree” substitute when the user asked to attach the zip.
