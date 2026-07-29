---
name: graphify
description: >
  Turn any folder of code/docs into a queryable knowledge graph (god nodes,
  communities, path/explain/query). Native ds-build reimplementation of
  Graphify-Labs/graphify. Use for architecture questions, file relationships,
  or when graphify-out/ exists. Triggers: /graphify, graphify, knowledge graph,
  "what connects", "how does X relate".
metadata:
  short-description: "Build & query a codebase knowledge graph"
---

# /graphify

Native Graphify-compatible knowledge graph for ds-build. Maps code + docs into
`graphify-out/` so you can **query instead of grepping**.

## Usage

```
/graphify                                             # full pipeline on .
/graphify <path>                                      # full pipeline on path
/graphify <path> --update                             # rebuild (incremental entrypoint)
/graphify <path> --cluster-only                       # recluster existing graph
/graphify <path> --no-viz                             # skip graph.html
/graphify query "<question>"                          # BFS scoped subgraph
/graphify path "A" "B"                                # shortest path
/graphify explain "Concept"                           # node + neighbors
```

## What you must do when invoked

### Help
If args are `--help` or `-h` only: print the Usage block above and stop.

### Fast path — existing graph
If `graphify-out/graph.json` exists **and** the user asked a natural-language
codebase question (not an explicit rebuild with `--update` / `--cluster-only` /
bare path rebuild):

1. Run `graphify query "<their question>"` (or `path` / `explain` when they
   named two nodes / one concept).
2. Answer from that output. Only open raw source after orienting via the graph.

### Full build
Otherwise:

1. Ensure the native CLI is available:
   ```bash
   command -v graphify || cargo run -p ds-graphify --bin graphify -- --help
   ```
   Prefer `graphify` on PATH (installed via cargo install / workspace build).
   If missing, invoke via `cargo run -p ds-graphify --bin graphify -- <args>`
   from the ds-build workspace root.

2. Run the build (default path `.`):
   ```bash
   graphify .
   # or: graphify <path> [--update] [--no-viz] [--cluster-only] [--resolution 1.5]
   ```

3. Summarize for the user:
   - Corpus line from CLI stdout
   - Node / edge / community counts
   - Paths to `graphify-out/GRAPH_REPORT.md`, `graph.json`, `graph.html`
   - Top god nodes + 2–3 suggested questions from the report

4. Do **not** ask for API keys. Structural (AST) extraction is fully local.
   Optional semantic JSON can be placed at `graphify-out/.graphify_semantic.json`
   (Graphify schema: `{nodes, edges}`) and will be merged on the next build.

### Query helpers
```bash
graphify query "what connects auth to the database?"
graphify path "UserService" "DatabasePool"
graphify explain "RateLimiter"
```

## Outputs

```
graphify-out/
├── graph.html       # open in browser — filter, click nodes
├── GRAPH_REPORT.md  # god nodes, communities, surprises, questions
└── graph.json       # full graph — query without re-reading files
```

## Confidence tags

Every edge is `EXTRACTED` (explicit in source), `INFERRED` (call-graph resolution),
or `AMBIGUOUS`. Prefer EXTRACTED when citing architecture facts.

## Languages (AST, free, local)

Rust, Python, Go, JavaScript, TypeScript (+ markdown docs, package manifests).
Other code extensions are detected but may contribute only file-level nodes until
extractors are added — still safe to run.

## Always-on guidance

When `graphify-out/graph.json` exists, for architecture / "how does X work" /
"what calls Y" questions: run `graphify query` **before** broad greps or
opening many source files.
