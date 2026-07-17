---
name: upstream-sync
description: >
  Review and selectively port updates from upstream xai-org/grok-build into
  this DS Build fork. Use when the user says upstream, grok-build update,
  port from grok, sync upstream, or /upstream-sync.
---

# Upstream sync (Grok Build → DS Build)

## Hard rules

1. **Never** `git merge` / `git rebase` / `git cherry-pick` upstream onto `main`
   without explicit user override. Histories diverge; crates are renamed.
2. **Never** re-enable xAI Mixpanel/Sentry phone-home, xAI OAuth, or replace
   DeepSeek defaults with Grok API defaults unless the user asks.
3. Port **behavior** with DS names (`ds_*`, `DS_HOME`, `~/.ds`). Do not leave
   `xai_*` / `xai-grok-*` identifiers in the tree.
4. Verify before claiming a port works: build (and tests when relevant).
5. Record decisions in the review dossier + advance the ledger with
   `mark-reviewed` when the cycle is done (or ask the user to).

## Canonical docs

- Workflow: `docs/upstream-sync.md`
- Path map: `upstream/path-map.md`
- State cursor: `upstream/state.json`
- Ledger: `upstream/LEDGER.md`
- Reviews: `upstream/reviews/*.md`
- CLI: `./scripts/upstream-sync.sh`

## Standard procedure

### A. Discover

```bash
./scripts/upstream-sync.sh setup   # once
./scripts/upstream-sync.sh fetch
./scripts/upstream-sync.sh status
./scripts/upstream-sync.sh review
```

Read the generated `upstream/reviews/<sha>.md`.

### B. Triage

Use the dossier classes:

| Class | Default |
|-------|---------|
| `PORT-HIGH` | Implement |
| `PORT-REVIEW` | Implement if relevant to DS users |
| `SKIP` | Skip (product/brand/phone-home) |
| `DEFER` | Skip for now; note in Decisions |

Prefer security and correctness fixes. Skip enterprise STT, billing URLs,
OAuth scope nits, branding-only, and telemetry.

### C. Port one item

1. Map path:
   ```bash
   ./scripts/upstream-sync.sh map-path <upstream-path>
   ```
2. Read mapped upstream source:
   ```bash
   ./scripts/upstream-sync.sh show <upstream-path> @upstream/main
   ```
3. Open the DS file. Diff intent. Apply the minimal correct change.
4. Build affected crates, e.g.:
   ```bash
   cargo build -p ds-pager-bin
   # or crate-local tests when the change is small
   ```
5. Update the **Decisions** table in the review markdown.

### D. Finish the cycle

```bash
./scripts/upstream-sync.sh mark-reviewed <sha> \
  --ported N --skipped N --deferred N \
  --note "short summary"
```

Only mark reviewed when triage is complete for that SHA (ports may be zero).

## Rename cheatsheet (apply longest-first)

Paths:

- `crates/codegen/xai-grok-*` → `crates/codegen/ds-*`
- `crates/codegen/xai-*` → `crates/codegen/ds-*`
- `crates/common/xai-grok-*` / `xai-*` → `crates/common/ds-*`
- `crates/build/xai-proto-build` → `crates/build/ds-proto-build`
- `prod/mc/cli-chat-proxy-types` → `prod/mc/cli-proxy-types`

Idents (script `map-text` / `show` already apply these):

- `xai_grok_` → `ds_`, `xai_` → `ds_`, `GROK_HOME` → `DS_HOME`, `~/.grok` → `~/.ds`

## User-facing summary format

When done, report:

1. Upstream tip SHA + how many commits/files pending or reviewed
2. Table of ported / skipped / deferred with one-line reasons
3. Build/test evidence for ports
4. Whether `mark-reviewed` was run
