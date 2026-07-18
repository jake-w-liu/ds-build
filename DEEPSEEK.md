# How to set up the DeepSeek API for DS

This is the **canonical setup guide** for this fork. Follow the steps in order.

---

## What you need

1. A DeepSeek account and API key  
2. A built `ds` binary (from this repo)  
3. Config **or** an environment variable with that key  

| | |
|--|--|
| Get API key | https://platform.deepseek.com/api_keys |
| API base URL | `https://api.deepseek.com/v1` |
| Models | `deepseek-v4-pro` (default), `deepseek-v4-flash` (fast) |
| API style | OpenAI-compatible **chat completions** |
| Docs | https://api-docs.deepseek.com/ |

Keys look like: `sk-...` (not `ds-...`).

---

## Step 1 — Create an API key

1. Open [platform.deepseek.com/api_keys](https://platform.deepseek.com/api_keys).
2. Sign in / create an account.
3. Create a new key and copy it (starts with `sk-`).
4. Keep it private; do not commit it to git.

---

## Step 2 — Put the key where `ds` can read it

Pick **one** of the options below. Interactive paste is recommended (CodeWhale-style).

### Option A — `ds auth set` (recommended)

After installing `ds`:

```bash
ds auth set
# paste your sk-… key (input is not echoed), then Enter
```

Or non-interactively:

```bash
printf 'sk-YOUR_KEY\n' | ds auth set --api-key-stdin
# discouraged (shell history):  ds auth set --api-key 'sk-…'
```

This writes the key to:

- top-level `api_key` and both `[model.deepseek-v4-pro]` / `[model.deepseek-v4-flash]` in `~/.ds/config.toml`
- `~/.ds/auth.json` (`ds::api_key` scope)
- `[auth] preferred_method = "api_key"`

Check / clear:

```bash
ds auth status    # never prints the full secret
ds auth get
ds auth clear
```

On first interactive launch, if no key is configured and stdin is a TTY, `ds`
prompts: **Enter DeepSeek API key…** (same paste flow).

### Option B — Config file (manual)

```bash
mkdir -p ~/.ds
cp /path/to/this/repo/config.example.toml ~/.ds/config.toml
```

Edit `~/.ds/config.toml` and replace **both** placeholders with your real key:

```toml
[auth]
preferred_method = "api_key"

[endpoints]
ds_api_base_url = "https://api.deepseek.com/v1"

# CORRECT: singular "model" — not "models"
[model.deepseek-v4-pro]
api_key = "sk-YOUR_KEY_HERE"
base_url = "https://api.deepseek.com/v1"
api_backend = "chat_completions"
context_window = 1000000

[model.deepseek-v4-flash]
api_key = "sk-YOUR_KEY_HERE"
base_url = "https://api.deepseek.com/v1"
api_backend = "chat_completions"
context_window = 1000000
```

**Common mistakes**

| Wrong | Right |
|-------|--------|
| `[models.deepseek-v4-pro]` | `[model.deepseek-v4-pro]` |
| Leaving `sk-PASTE_...` placeholder | Paste the real `sk-...` key |
| Different broken key only on flash | Use the **same valid** key for both models |
| Committing `~/.ds/config.toml` into the repo | Keep it only under `~/.ds/` |

Then lock down the file:

```bash
chmod 600 ~/.ds/config.toml
```

### Option C — Environment variable

```bash
export DEEPSEEK_API_KEY="sk-YOUR_KEY_HERE"
```

Optional aliases (lower priority):

```bash
export DS_API_KEY="sk-YOUR_KEY_HERE"
# or (legacy)
export DS_CODE_API_KEY="sk-YOUR_KEY_HERE"
```

To make it permanent in zsh:

```bash
echo 'export DEEPSEEK_API_KEY="sk-YOUR_KEY_HERE"' >> ~/.zshrc
source ~/.zshrc
```

Prefer the config file if you do not want the key in shell history.

### How `ds` resolves the key (priority order)

1. `DEEPSEEK_API_KEY`  
2. `DS_API_KEY`  
3. `DS_CODE_API_KEY`  
4. Top-level `api_key` in `~/.ds/config.toml` (from `ds auth set`)  
5. First non-empty `api_key` under **`[model.*]`** in `~/.ds/config.toml`  
   (then project `.ds/config.toml` if present)  
6. `ds::api_key` scope in `~/.ds/auth.json`

---

## Step 3 — Install `ds` (if you have not already)

From the **repository root**:

```bash
cargo build -p ds-pager-bin --release
install -m 755 target/release/ds-pager ~/.local/bin/ds
codesign --force --sign - ~/.local/bin/ds   # macOS: avoid Gatekeeper SIGKILL

# PATH (once)
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc

ds --version   # expect: ds <current semver> (<git commit>)
ds auth status # after configuring a key
```

---

## Step 4 — Verify the API works

Headless smoke test (does not open the TUI):

```bash
ds -p "Reply with exactly: DEEPSEEK_OK" \
  --model deepseek-v4-flash \
  --always-approve \
  --max-turns 2 \
  --output-format plain
```

Expected stdout:

```text
DEEPSEEK_OK
```

Interactive TUI:

```bash
ds
```

Then type a short prompt and confirm a normal reply.

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Not signed in` / login prompt | Key not found. Run `ds auth set` / `ds auth status`, or check `echo $DEEPSEEK_API_KEY` / `[model.*]` in `~/.ds/config.toml`. |
| `Unauthorized (401)` / invalid API key | Wrong or revoked key. Create a new key at platform.deepseek.com and paste again (both models if using config). |
| Key in config ignored | You used `[models....]` (plural). Change to `[model....]`. |
| Works for pro, fails for flash | Flash entry has a bad key; copy the working pro `api_key` into the flash block. |
| `command not found: ds` | Install binary and ensure `~/.local/bin` is on `PATH`. |
| Still seeing old system prompt / identity | Start a **new** session; rebuild after prompt changes. |

---

## Optional config knobs

Already set in `config.example.toml` for this fork:

```toml
[ui]
permission_mode = "always-approve"   # use "ask" to prompt for tools

[subagents]
enabled = true                       # false or --no-subagents to disable

[features]
telemetry = false
```

---

## Privacy note

- **DeepSeek chat API** = required for the agent to work (your key, your usage).  
- **xAI / Mixpanel / Sentry product phone-home** = disabled in this fork.  

---

## Related

- [`README.md`](README.md) — build & overview  
- [`config.example.toml`](config.example.toml) — full template to copy  
- User guide: `crates/codegen/ds-pager/docs/user-guide/`  
