# Upstream port ledger

One row per upstream SHA that was **reviewed**. Port outcomes live in the
per-SHA review file under `upstream/reviews/`.

| Reviewed (UTC) | Upstream SHA | Summary | Ported | Skipped | Deferred | Review file |
|----------------|--------------|---------|--------|---------|----------|-------------|
| _(none yet)_ | | | | | | |

## How to update

After `./scripts/upstream-sync.sh review` and finishing a port cycle:

```bash
./scripts/upstream-sync.sh mark-reviewed <sha> \
  --ported N --skipped N --deferred N \
  --note "short summary"
```

That appends a row here and advances `upstream/state.json`.
