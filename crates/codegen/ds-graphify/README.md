# ds-graphify

Native, Graphify-compatible knowledge-graph pipeline for ds-build.

Architecture mirrors [Graphify-Labs/graphify](https://github.com/Graphify-Labs/graphify):

```
detect → extract → build → cluster → analyze → report → export
```

## Build / install CLI

```bash
cargo build -p ds-graphify --bin graphify --release
# binary: target/release/graphify
```

## Usage

```bash
graphify .                         # full pipeline → graphify-out/
graphify query "how does auth work"
graphify path "AuthService" "Db"
graphify explain "AuthService"
graphify update .                  # rebuild entrypoint
graphify . --cluster-only          # recluster existing graph.json
graphify . --no-viz                # skip graph.html
```

Outputs under `graphify-out/`:

| File | Purpose |
|------|---------|
| `graph.json` | NetworkX node-link graph (`links` + confidence tags) |
| `GRAPH_REPORT.md` | God nodes, communities, surprises, questions |
| `graph.html` | Interactive force-directed view (vis-network) |

## Slash command

Bundled skill `/graphify` is shipped via `ds-shell` (`skills/graphify/SKILL.md`)
and extracted to `~/.ds/skills/graphify/` on startup.

## Languages (AST, local, free)

Rust, Python, Go, JavaScript, TypeScript, Markdown docs, package manifests.

Optional LLM semantic extractions can be merged from
`graphify-out/.graphify_semantic.json` (same node/edge schema).

## Relation to ds-codebase-graph

`ds-codebase-graph` is an LSP-style scope index (goto-def/refs).
`ds-graphify` is a knowledge graph (god nodes, communities, query/path/explain).
They share tree-sitter but serve different products.
