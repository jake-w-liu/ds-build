---
name: upstream-sync
description: >
  Full upstream port cycle for xai-org/grok-build into DS Build: fetch, triage,
  implement every verified item, verify correctness, mark reviewed, patch-bump,
  push main, release-rebuild dual install. Use for /upstream-sync, upstream,
  grok-build update, port from grok.
---

# /upstream-sync — implement verified upstream fixes

## What this skill does (end-to-end)

When the user runs **`/upstream-sync`** (or equivalent), **do not stop at a
review doc**. Run the full cycle:

1. **Fetch** upstream and see what is pending  
2. **Triage** changes (auto classes + judgment)  
3. **Implement every verified item** (default: all `PORT-HIGH`; also
   `PORT-REVIEW` when clearly relevant to DS)  
4. **Check correctness** — build and targeted tests; fix regressions before
   claiming done  
5. **Commit ports** (if any) and **mark reviewed** so the next run only sees
   newer upstream commits  
6. **If anything was ported** (or the user asked to ship): **patch-bump,
   push `main`, release-rebuild and dual-install** via
   `./bump-and-install.sh` so installed `ds` matches the new tip  
7. **Report** ported / skipped / deferred + verification + new version /
   `ds --version`  

Ephemeral reviews and cursors live under **`~/.ds/upstream-sync/`** (not in
git). In-repo surface: this skill, `scripts/upstream-sync.sh`,
`./bump-and-install.sh`.

## Hard rules

1. **Never** `git merge` / `rebase` / `cherry-pick` upstream onto `main`
   unless the user explicitly overrides. Histories and crate names diverge.
2. **Never** re-enable xAI Mixpanel/Sentry phone-home, xAI OAuth, or replace
   DeepSeek defaults with Grok API defaults unless the user asks.
3. Port **behavior** with DS names (`ds_*`, `DS_HOME`, `~/.ds`). No leftover
   `xai_*` / `xai-grok-*` in the tree.
4. **Verify before assert.** A port is not done until build (and relevant
   tests) succeed. Quote the command + outcome.
5. Do **not** commit review dossiers, ledgers, or path-map markdown into the
   repo. Keep the tree free of sync artifacts.

## CLI (helper only)

```bash
./scripts/upstream-sync.sh setup     # once: add remote + ~/.ds state
./scripts/upstream-sync.sh fetch
./scripts/upstream-sync.sh status
./scripts/upstream-sync.sh review    # → ~/.ds/upstream-sync/reviews/<sha>.md
./scripts/upstream-sync.sh map-path <upstream/path>
./scripts/upstream-sync.sh show <upstream/path> [@rev]
./scripts/upstream-sync.sh mark-reviewed <sha> --ported N --skipped N --deferred N --note "…"
```

## Triage classes

| Class | Meaning | Action on `/upstream-sync` |
|-------|---------|----------------------------|
| `PORT-HIGH` | Security, crash, correctness, sandbox | **Must implement** |
| `PORT-REVIEW` | Likely useful, needs judgment | **Implement if relevant to DS** |
| `SKIP` | xAI product / OAuth / billing / STT / branding / phone-home | Skip |
| `DEFER` | Large pure refactor / docs-only churn | Skip unless user asks |

### Skip keywords (non-exhaustive)

OAuth scopes, enterprise STT, billing URLs, Mixpanel, Sentry, xAI voice WSS,
branding-only, welcome logo, Grok model catalog.

### High-priority keywords

`security`, SSRF, sandbox, crash, hang, leak, race, `fix(`, headless drain,
invariants, authz.

## Rename map (longest first)

**Paths**

- `crates/codegen/xai-grok-*` → `crates/codegen/ds-*`
- `crates/codegen/xai-*` → `crates/codegen/ds-*`
- `crates/common/xai-grok-*` / `xai-*` → `crates/common/ds-*`
- `crates/build/xai-proto-build` → `crates/build/ds-proto-build`
- `prod/mc/cli-chat-proxy-types` → `prod/mc/cli-proxy-types`

**Idents** (`show` / `map-text` apply these)

- `xai_grok_` → `ds_`, `xai_` → `ds_`, `GROK_HOME` → `DS_HOME`, `~/.grok` → `~/.ds`

## Implementation loop (per verified item)

1. Map path + `show` mapped upstream blob.  
2. Open DS file; port the **minimal behavioral change** (not blind overwrite).  
3. Keep DS product defaults.  
4. **Correctness check** for that change:
   - Prefer crate-scoped tests when they exist and are cheap.  
   - At minimum: `cargo build -p ds-pager-bin` (or the affected package) must
     succeed after the batch.  
   - If a test fails, fix or revert that port — do not leave red builds.  
5. Note the item as ported with file paths.

## Finish

```bash
./scripts/upstream-sync.sh mark-reviewed <sha> \
  --ported N --skipped N --deferred N \
  --note "one-line summary"
```

### Ship: bump + push + rebuild (required when ports landed)

When **at least one item was ported**, after ports are committed and the
tree is clean (except intentional untracked files), run the product ship
path so crates, lockfile, remote, and both install paths stay lockstepped:

```bash
# Working tree must be clean of tracked changes (bump script enforces this).
./bump-and-install.sh
```

That script:

1. Bumps the product **patch** on every first-party crate  
2. Release-builds with `DS_VERSION`  
3. Commits `chore: bump to vX.Y.Z` and **pushes `origin/main`**  
4. Rebuilds so the baked git SHA matches the bump commit  
5. Installs `~/.local/bin/ds` and `~/.ds/bin/ds` (codesign on macOS)  
6. Verifies crate versions + both binaries report the new version  

If **zero** items were ported (review-only / all skip), do **not** bump
unless the user explicitly asks.

Do **not** hand-edit a few `Cargo.toml` versions — always use
`./bump-and-install.sh` so nothing drifts.

## User-facing report (required)

| Upstream SHA | … |
| Ported | item → DS paths |
| Skipped | item → reason |
| Deferred | item → reason |
| Verification | commands run + pass/fail |
| Ship | bump commit / version / `ds --version` (or “no bump — nothing ported”) |

Do not claim “everything is up to date” unless `status` shows no pending
commits after `mark-reviewed`, verification passed, and (when ports
landed) bump + dual install succeeded.
