# Local Qwen (MLX) for DS Build

This machine can run
[`orcarouter/Qwen3.8-27B-Uncensored-MLX`](https://huggingface.co/orcarouter/Qwen3.8-27B-Uncensored-MLX)
on Apple Silicon and point `ds` at it over the OpenAI Chat Completions API.

Only **4 / 6 / 8-bit** are wired up. 2-bit is omitted (the card marks it
severely degraded).

| Quant | Disk | This 32 GB Mac |
|-------|------|----------------|
| **4-bit** | ~15 GB | Recommended default |
| **6-bit** | ~22 GB | Tight; close other apps |
| **8-bit** | ~27.5 GB | Likely to swap |

`ds local setup` / `ds local serve` **auto-size** `context_window` from
physical RAM, on-disk weight size, and this model's KV cost (16 full-attention
layers x 4 KV heads x 256 dim x bf16). The weights advertise 262144; that
cache will not fit. DeepSeek's 1M window is not inherited.

On this 32 GB Mac the planner currently writes about **49k / 16k / 4k** for
4 / 6 / 8-bit (ladder-snapped, cap 65536). `ds local serve` also passes
`--max-kv-size` so mlx-vlm cannot grow past that.

## Layout

| Path | Role |
|------|------|
| `~/.ds/local-mlx/venv` | Python venv with `mlx>=0.32`, `mlx-vlm>=0.6.13`, `huggingface_hub` |
| `~/models/Qwen3.8-27B-Uncensored-MLX/{4,6,8}-bit` | Weights |
| `~/.ds/local-mlx/state.json` | Last serve bits / host / port |
| `~/.ds/local-mlx/server.pid` | PID of a server started by `ds local serve` |
| `~/.ds/local-mlx/server.log` | Server stdout/stderr |

## One-time

The Hugging Face repo is **gated**. Accept the terms on the model page,
then export a **read** token:

```sh
export HF_TOKEN=hf_...
```

```sh
# venv (already created on this Mac if setup ran)
python3 -m venv ~/.ds/local-mlx/venv
~/.ds/local-mlx/venv/bin/pip install -U 'mlx>=0.32' 'mlx-vlm>=0.6.13' 'huggingface_hub[cli,hf_xet]' jinja2

ds local setup                 # write [model.qwen3-8-27b-{4,6,8}bit]
ds local download              # 4 + 6 + 8 (or: --bits 4)
ds local serve --bits 4        # OpenAI-compatible http://127.0.0.1:8080/v1
ds --model qwen3-8-27b-4bit    # or /model qwen3-8-27b-4bit in the TUI
```

`ds local use 4` sets `[models].default` and restarts a running server.

In the TUI:

```
/model qwen3-8-27b-4bit
/model Qwen 4-bit (local)
```

Local models use a **compact** system prompt (CRC/verification, no Fable
preamble). `/fable` turns the method on for that session.

## Commands

```
ds local setup [--default-bits 4]
ds local download [--bits 4]
ds local serve [--bits 4] [--port 8080] [--foreground]
ds local stop
ds local status [--json]
ds local use 4|6|8
```

`ds auth set` / `ds auth clear` do **not** retarget loopback models at
DeepSeek. A session whose default model is loopback does not require a
DeepSeek API key.

## Notes

- Serve **one** quant at a time. 32 GB cannot hold two 27B MLX loads.
- First load after `serve` can take several minutes (weights + Metal compile).
- The server is `python -m mlx_vlm server` (vision architecture
  `Qwen3_5ForConditionalGeneration`). Do not use `mlx_lm.server` for this repo.
- `ds local stop` only signals the PID recorded in `server.pid` after
  checking that process is an `mlx_vlm` server.
