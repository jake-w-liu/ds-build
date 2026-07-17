# Upstream → DS path & identifier map

Grok Build crate dirs use `xai-` / `xai-grok-` prefixes. DS Build uses `ds-`.
Apply **directory renames first**, then **identifier renames** when reading
upstream blobs or rewriting patches.

## Directory renames (apply in order)

| Upstream path prefix | DS path prefix |
|----------------------|----------------|
| `crates/build/xai-proto-build` | `crates/build/ds-proto-build` |
| `crates/codegen/xai-grok-` | `crates/codegen/ds-` |
| `crates/codegen/xai-` | `crates/codegen/ds-` |
| `crates/common/xai-grok-` | `crates/common/ds-` |
| `crates/common/xai-` | `crates/common/ds-` |
| `prod/mc/cli-chat-proxy-types` | `prod/mc/cli-proxy-types` |

Unchanged: `crates/codegen/ptyctl`, `crates/codegen/ptyctl-cli`, `third_party/`,
most root config files (`rust-toolchain.toml`, `clippy.toml`, …).

## Crate / Rust identifier renames (apply in order)

| Upstream | DS |
|----------|-----|
| `xai_grok_` | `ds_` |
| `xai-grok-` | `ds-` |
| `xai_grok` | `ds` |
| `XaiGrok` | `Ds` |
| `xai_` | `ds_` |
| `xai-` | `ds-` |
| `::xai` | `::ds` |
| `GROK_HOME` | `DS_HOME` |
| `~/.grok` | `~/.ds` |
| `/.grok/` | `/.ds/` |
| `grok-build` | `ds-build` (docs only — careful) |

## Binary / product renames

| Upstream | DS |
|----------|-----|
| binary `grok` / package `xai-grok-pager-bin` | `ds` / `ds-pager-bin` |
| CLI product name Grok Build | DS Build |
| default API xAI | DeepSeek (`api.deepseek.com`) |

## Default skip classes (usually not ported)

- xAI OAuth scopes, enterprise STT / voice WSS, billing portal URLs
- Mixpanel / Sentry / product telemetry phone-home
- Branding, welcome copy, model catalog for Grok-only models
- Update channels that only exist on xAI distribution
- Anything that re-enables network phone-home this fork disabled

## Default high-priority classes (usually port)

- Security fixes (`security:`, SSRF, sandbox, authz)
- Crash / hang / data-loss fixes
- Correctness bugs in pager, shell, tools, hooks, MCP
- Robustness improvements with no xAI product dependency
