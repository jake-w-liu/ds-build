# DS (`@ds-official/ds` package layout)

npm packaging layout for the **DS** CLI binary. This monorepo fork is meant to
be **built from source**; the npm package names/URLs below are layout remnants
and are **not** a published registry product for this tree.

## Recommended install (from this repo)

```bash
# repo root
cargo build -p ds-pager-bin --release
install -m 755 target/release/ds-pager ~/.local/bin/ds

export PATH="$HOME/.local/bin:$PATH"
ds --version   # 0.1.0
```

API key: see the root [`DEEPSEEK.md`](../../../../../DEEPSEEK.md) and
[`config.example.toml`](../../../../../config.example.toml).

```bash
# interactive
ds

# headless
ds -p "Explain this codebase" --model deepseek-v4-flash
```

Authenticate with a DeepSeek API key from
[platform.deepseek.com/api_keys](https://platform.deepseek.com/api_keys)
(via `DEEPSEEK_API_KEY` or `~/.ds/config.toml` under `[model.*]`).

## Supported platforms (native binaries)

| Platform | Architecture |
|----------|----------------|
| macOS | Apple Silicon (arm64), x86_64 |
| Linux | x86_64, arm64 |
| Windows | x86_64, arm64 |

Platform package directories live next to this folder (`ds-darwin-arm64`, etc.).

## Docs

- Root overview: repository [`README.md`](../../../../../README.md)
- DeepSeek setup: [`DEEPSEEK.md`](../../../../../DEEPSEEK.md)
- User guide: [`docs/user-guide/`](../../docs/user-guide/)
