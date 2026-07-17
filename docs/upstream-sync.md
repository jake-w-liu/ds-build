# Upstream sync workflow (Grok Build → DS Build)

DS Build is a **rebranded, independently-historied** fork of
[xai-org/grok-build](https://github.com/xai-org/grok-build). Upstream pushes
periodic **“Synced from monorepo”** commits (often one large squash with a
bullet list of fixes). We do **not** `git merge` those commits.

Instead we run a selective port pipeline:

```
fetch → status → review → classify → implement relevant ports → mark-reviewed
```

Tooling lives in:

| Path | Role |
|------|------|
| `scripts/upstream-sync.sh` | CLI for the pipeline |
| `upstream/state.json` | Last reviewed / ported SHA cursor |
| `upstream/LEDGER.md` | Human-readable history of reviews |
| `upstream/reviews/<sha>.md` | Per-SHA dossier + decisions |
| `upstream/path-map.md` | Path & identifier rename rules |

## One-time setup

```bash
./scripts/upstream-sync.sh setup
./scripts/upstream-sync.sh fetch
./scripts/upstream-sync.sh status
```

## Regular cycle (when upstream moves)

### 1. Fetch & see what’s new

```bash
./scripts/upstream-sync.sh fetch
./scripts/upstream-sync.sh status
```

### 2. Generate a review dossier

```bash
./scripts/upstream-sync.sh review
# → upstream/reviews/<shortsha>.md
```

The dossier includes:

- Commit list + full messages (including monorepo “Changes:” bullets)
- Auto **triage class** per bullet: `PORT-HIGH` / `PORT-REVIEW` / `SKIP` / `DEFER`
- Every changed file with **mapped DS path** and whether it exists locally
- Diffstat + a port checklist

### 3. Study relevance

Read the dossier. Default policy:

| Class | Meaning | Action |
|-------|---------|--------|
| `PORT-HIGH` | Security, crash, correctness | Port unless proven DS-irrelevant |
| `PORT-REVIEW` | Likely useful, needs judgment | Port if it helps DS users |
| `SKIP` | xAI product / OAuth / billing / phone-home / branding | Do not port |
| `DEFER` | Large pure refactor / docs churn | Park for a later focused effort |

See also `upstream/path-map.md` for rename rules and hard skip classes.

### 4. Implement relevant ports

For each file you choose to port:

```bash
# View upstream blob rewritten into DS naming
./scripts/upstream-sync.sh show crates/codegen/xai-grok-shell/src/foo.rs @upstream/main \
  | less

# Map a path
./scripts/upstream-sync.sh map-path crates/codegen/xai-grok-pager/src/app/cli.rs
# → crates/codegen/ds-pager/src/app/cli.rs
```

Port **behavior**, not blind file copies:

1. Open the mapped DS file.
2. Diff intent against the mapped upstream text.
3. Keep DS defaults (DeepSeek models, `DS_HOME`, no xAI phone-home).
4. Build: `cargo build -p ds-pager-bin --release` (or crate-scoped tests).
5. Fill the **Decisions** table in the review dossier.

Agent assist: load the **upstream-sync** skill and ask to work a specific
dossier item (e.g. “port the SSRF hook-runner fix from review 8adf901”).

### 5. Mark reviewed (advance the cursor)

Even if you skipped everything, mark the SHA so the next cycle only sees
newer commits:

```bash
./scripts/upstream-sync.sh mark-reviewed 8adf901 \
  --ported 3 --skipped 5 --deferred 1 \
  --note "ported SSRF + headless drain; skipped enterprise STT"
```

This updates `upstream/state.json` and appends a row to `upstream/LEDGER.md`.

## Why not merge?

| | Merge upstream | This workflow |
|--|----------------|---------------|
| History | Divergent renames explode conflicts | Independent commits |
| Brand | Re-introduces `xai_*`, `~/.grok` | Explicit map to `ds_*` / `~/.ds` |
| Product | Pulls xAI OAuth / billing / telemetry | Classify & skip |
| Intent | Opaque giant squash | Bullet triage + decisions ledger |

## Commands cheat sheet

```text
setup            Add/update the `upstream` git remote
fetch            git fetch upstream main
status           Tip vs last_reviewed_sha
review [--sha]   Write upstream/reviews/<sha>.md
map-path PATH    Print DS path for an upstream path
map-text         stdin → identifier-renamed stdout
show PATH [@rev] Print mapped upstream file contents
changed-files    name-status list for pending range
mark-reviewed    Advance cursor + ledger row
```

## First run expectation

Until the first `mark-reviewed`, `status` reports **no baseline**. The first
`review` uses the tip commit’s parent as the left side of the range (typical
for monorepo sync squashes). After that, each cycle is `last_reviewed..tip`.
