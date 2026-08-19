//! `ds local` — serve and register a local OpenAI-compatible MLX model.
//!
//! Targets the OrcaRouter Qwen3.8-27B Uncensored MLX quants (4 / 6 / 8-bit)
//! via `mlx-vlm`'s OpenAI-compatible server. 2-bit is intentionally omitted.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

const HF_REPO: &str = "orcarouter/Qwen3.8-27B-Uncensored-MLX";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_BITS: u8 = 4;
const SUPPORTED_BITS: [u8; 3] = [4, 6, 8];
const DUMMY_API_KEY: &str = "local";
const IDLE_TIMEOUT_SECS: i64 = 600;
const HEALTH_WAIT: Duration = Duration::from_secs(300);

/// Smallest window we will write. Below this, a coding-agent turn cannot
/// hold tools + compact prompt + a reply.
const MIN_CONTEXT_WINDOW: u64 = 4_096;
/// Local 27B cap. The card advertises 262144; that KV cache will not fit
/// in 32-64 GB unified memory once weights are loaded.
const MAX_LOCAL_CONTEXT_WINDOW: u64 = 65_536;
const MIN_COMPLETION_TOKENS: u64 = 1_024;
const MAX_COMPLETION_TOKENS: u64 = 8_192;
const WINDOW_LADDER: &[u64] = &[4_096, 8_192, 12_288, 16_384, 24_576, 32_768, 49_152, 65_536];

#[derive(Debug, clap::Args, Clone)]
pub struct LocalArgs {
    #[command(subcommand)]
    pub command: LocalCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum LocalCommand {
    /// Write local Qwen model entries into `~/.ds/config.toml`
    Setup {
        /// Also set `[models].default` to this quant (4, 6, or 8)
        #[arg(long)]
        default_bits: Option<u8>,
    },
    /// Download 4 / 6 / 8-bit weights from Hugging Face (skips 2-bit)
    Download {
        /// Restrict to one quant (4, 6, or 8). Default: all three.
        #[arg(long)]
        bits: Option<u8>,
    },
    /// Start the local mlx-vlm OpenAI-compatible server
    Serve {
        /// Quant to load: 4 (recommended on 32 GB), 6, or 8
        #[arg(long)]
        bits: Option<u8>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        /// Stay in the foreground instead of daemonizing
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the server started by `ds local serve`
    Stop,
    /// Show download / server / config status
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Point `[models].default` at a local quant and restart the server if it is up
    Use {
        /// Quant to use: 4, 6, or 8
        bits: u8,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalState {
    bits: u8,
    host: String,
    port: u16,
    model_dir: String,
    #[serde(default)]
    pid: Option<u32>,
}

impl Default for LocalState {
    fn default() -> Self {
        Self {
            bits: DEFAULT_BITS,
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            model_dir: default_model_root().display().to_string(),
            pid: None,
        }
    }
}

pub fn run(args: LocalArgs) -> Result<()> {
    match args.command {
        LocalCommand::Setup { default_bits } => cmd_setup(default_bits),
        LocalCommand::Download { bits } => cmd_download(bits),
        LocalCommand::Serve {
            bits,
            port,
            host,
            foreground,
        } => cmd_serve(bits, port, host, foreground),
        LocalCommand::Stop => cmd_stop(),
        LocalCommand::Status { json } => cmd_status(json),
        LocalCommand::Use { bits } => cmd_use(bits),
    }
}

fn parse_bits(bits: u8) -> Result<u8> {
    if SUPPORTED_BITS.contains(&bits) {
        Ok(bits)
    } else {
        bail!("unsupported bits={bits}; use 4, 6, or 8 (2-bit is omitted as too degraded)")
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/jake"))
}

fn ds_home() -> PathBuf {
    std::env::var_os("DS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".ds"))
}

fn state_dir() -> PathBuf {
    ds_home().join("local-mlx")
}

fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

fn pid_path() -> PathBuf {
    state_dir().join("server.pid")
}

fn log_path() -> PathBuf {
    state_dir().join("server.log")
}

fn venv_python() -> PathBuf {
    state_dir().join("venv/bin/python")
}

fn hf_bin() -> PathBuf {
    state_dir().join("venv/bin/hf")
}

fn default_model_root() -> PathBuf {
    std::env::var_os("DS_LOCAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("models/Qwen3.8-27B-Uncensored-MLX"))
}

fn quant_dir(root: &Path, bits: u8) -> PathBuf {
    root.join(format!("{bits}-bit"))
}

fn model_id(bits: u8) -> String {
    // Hyphens only: TOML `[model.qwen3.8-…]` would nest as model.qwen3.8-…
    format!("qwen3-8-27b-{bits}bit")
}

/// Architecture fields that drive KV-cache bytes/token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelArch {
    max_position_embeddings: u64,
    num_hidden_layers: u64,
    num_key_value_heads: u64,
    head_dim: u64,
    full_attention_layers: u64,
}

impl ModelArch {
    /// Defaults for this Qwen3.8-27B MLX repo (`text_config` in config.json).
    fn qwen38_27b() -> Self {
        Self {
            max_position_embeddings: 262_144,
            num_hidden_layers: 64,
            num_key_value_heads: 4,
            head_dim: 256,
            full_attention_layers: 16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalContextPlan {
    context_window: u64,
    max_completion_tokens: u64,
    thinking_budget: u64,
    kv_bytes_per_token: u64,
    weight_bytes: u64,
    ram_bytes: u64,
    model_max: u64,
    tight: bool,
}

fn parse_model_arch(v: &serde_json::Value) -> ModelArch {
    let mut arch = ModelArch::qwen38_27b();
    let text = v.get("text_config").unwrap_or(v);
    if let Some(n) = text
        .get("max_position_embeddings")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
    {
        arch.max_position_embeddings = n;
    }
    if let Some(n) = text
        .get("num_hidden_layers")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
    {
        arch.num_hidden_layers = n;
    }
    if let Some(n) = text
        .get("num_key_value_heads")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
    {
        arch.num_key_value_heads = n;
    }
    if let Some(n) = text
        .get("head_dim")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
    {
        arch.head_dim = n;
    }
    if let Some(layers) = text
        .get("layer_types")
        .and_then(serde_json::Value::as_array)
    {
        let n_full = layers
            .iter()
            .filter(|t| t.as_str() == Some("full_attention"))
            .count() as u64;
        if n_full > 0 {
            arch.full_attention_layers = n_full;
        } else if !layers.is_empty() {
            arch.full_attention_layers = arch.num_hidden_layers;
        }
    }
    arch
}

fn read_model_arch(quant_dir: &Path) -> ModelArch {
    let path = quant_dir.join("config.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return ModelArch::qwen38_27b();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ModelArch::qwen38_27b();
    };
    parse_model_arch(&v)
}

/// Conservative on-disk size for this 27B MLX repo when weights are not
/// downloaded yet (rounded up from the measured shards).
fn typical_weight_bytes(bits: u8) -> u64 {
    match bits {
        4 => 15 << 30,
        6 => 22 << 30,
        8 => 28 << 30,
        _ => 15 << 30,
    }
}

fn quant_weight_bytes(dir: &Path, bits: u8) -> u64 {
    let mut sum = 0u64;
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            if name.to_string_lossy().ends_with(".safetensors") {
                if let Ok(meta) = ent.metadata() {
                    sum = sum.saturating_add(meta.len());
                }
            }
        }
    }
    if sum == 0 {
        typical_weight_bytes(bits)
    } else {
        sum
    }
}

fn physical_ram_bytes() -> u64 {
    physical_ram_bytes_impl().unwrap_or(32 << 30)
}

#[cfg(target_os = "macos")]
fn physical_ram_bytes_impl() -> Option<u64> {
    let name = std::ffi::CString::new("hw.memsize").ok()?;
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut val as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 && val > 0 {
        Some(val)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn physical_ram_bytes_impl() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("MemTotal:") else {
            continue;
        };
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        return Some(kb.saturating_mul(1024));
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn physical_ram_bytes_impl() -> Option<u64> {
    None
}

fn ram_reserve(ram_bytes: u64) -> u64 {
    // 20% of RAM, floored at 4 GiB (OS + Metal) and capped at 12 GiB.
    (ram_bytes / 5).clamp(4 << 30, 12 << 30)
}

fn kv_bytes_per_token(arch: &ModelArch) -> u64 {
    let n_full = if arch.full_attention_layers > 0 {
        arch.full_attention_layers
    } else {
        arch.num_hidden_layers.max(1)
    };
    // K+V, bf16/fp16. Linear-attention layers are O(1) state, not O(seq).
    2u64.saturating_mul(n_full)
        .saturating_mul(arch.num_key_value_heads.max(1))
        .saturating_mul(arch.head_dim.max(1))
        .saturating_mul(2)
}

fn snap_window(raw: u64, cap: u64) -> u64 {
    let cap = cap.clamp(MIN_CONTEXT_WINDOW, MAX_LOCAL_CONTEXT_WINDOW);
    if raw >= cap {
        return cap;
    }
    WINDOW_LADDER
        .iter()
        .copied()
        .rev()
        .find(|&w| w <= raw && w <= cap)
        .unwrap_or(MIN_CONTEXT_WINDOW)
        .min(cap)
}

fn plan_context(ram_bytes: u64, weight_bytes: u64, arch: &ModelArch) -> LocalContextPlan {
    let ram_bytes = ram_bytes.max(1);
    let kv_bpt = kv_bytes_per_token(arch).max(1);
    let leftover = ram_bytes
        .saturating_sub(weight_bytes)
        .saturating_sub(ram_reserve(ram_bytes));
    // 35% of leftover for KV; the rest is prefill activations / fragmentation.
    let kv_budget = leftover.saturating_mul(7) / 20;
    let raw = kv_budget / kv_bpt;
    let model_max = arch.max_position_embeddings.max(MIN_CONTEXT_WINDOW);
    let cap = model_max.min(MAX_LOCAL_CONTEXT_WINDOW);
    let context_window = snap_window(raw, cap);
    let mut max_completion_tokens =
        (context_window / 4).clamp(MIN_COMPLETION_TOKENS, MAX_COMPLETION_TOKENS);
    let prompt_floor = MIN_CONTEXT_WINDOW / 2;
    if context_window > prompt_floor {
        max_completion_tokens = max_completion_tokens.min(context_window - prompt_floor);
    }
    max_completion_tokens = max_completion_tokens.max(256);
    let thinking_budget = (max_completion_tokens / 4)
        .max(256)
        .min(max_completion_tokens / 2)
        .max(1);
    LocalContextPlan {
        context_window,
        max_completion_tokens,
        thinking_budget,
        kv_bytes_per_token: kv_bpt,
        weight_bytes,
        ram_bytes,
        model_max,
        tight: leftover == 0 || raw < MIN_CONTEXT_WINDOW,
    }
}

fn plan_for_quant(root: &Path, bits: u8) -> LocalContextPlan {
    let dir = quant_dir(root, bits);
    let arch = read_model_arch(&dir);
    let weights = quant_weight_bytes(&dir, bits);
    plan_context(physical_ram_bytes(), weights, &arch)
}

fn load_state() -> LocalState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &LocalState) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    fs::write(
        state_path(),
        serde_json::to_string_pretty(state).context("serialize local-mlx state")?,
    )?;
    Ok(())
}

fn read_owned_pid() -> Option<u32> {
    let raw = fs::read_to_string(pid_path()).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if pid_is_our_server(pid) {
        Some(pid)
    } else {
        None
    }
}

fn pid_is_our_server(pid: u32) -> bool {
    if !ds_shell::util::is_process_alive(pid) {
        return false;
    }
    let path = format!("/proc/{pid}/cmdline");
    if let Ok(bytes) = fs::read(&path) {
        let text = String::from_utf8_lossy(&bytes);
        return text.contains("mlx_vlm");
    }
    // macOS: `ps` is the portable cmdline check.
    let Ok(out) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return false;
    };
    let cmd = String::from_utf8_lossy(&out.stdout);
    cmd.contains("mlx_vlm")
}

fn quant_ready(root: &Path, bits: u8) -> bool {
    let dir = quant_dir(root, bits);
    dir.join("config.json").is_file()
        && fs::read_dir(&dir)
            .ok()
            .map(|it| {
                it.filter_map(|e| e.ok()).any(|e| {
                    e.path()
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|ext| ext == "safetensors")
                })
            })
            .unwrap_or(false)
}

fn cmd_setup(default_bits: Option<u8>) -> Result<()> {
    if let Some(b) = default_bits {
        parse_bits(b)?;
    }
    let config_path = ds_home().join("config.toml");
    let Some(mut doc) = crate::config_toml_edit::read_config_document_for_edit(&config_path) else {
        bail!(
            "refusing to edit unparseable config: {}",
            config_path.display()
        );
    };

    let state = load_state();
    let port = state.port;
    let model_root = PathBuf::from(&state.model_dir);
    let base_url = format!("http://{DEFAULT_HOST}:{port}/v1");

    {
        let model_item = doc
            .entry("model")
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        let table = model_item
            .as_table_mut()
            .context("[model] is not a table")?;

        for bits in SUPPORTED_BITS {
            let id = model_id(bits);
            let entry = table
                .entry(&id)
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
            let model = entry
                .as_table_mut()
                .with_context(|| format!("[model.{id}] is not a table"))?;
            // mlx-vlm treats `model` as a Hugging Face repo id unless it is a
            // local path. Route each catalog entry at its quant directory.
            model["model"] = toml_edit::value(quant_dir(&model_root, bits).display().to_string());
            model["base_url"] = toml_edit::value(&base_url);
            model["name"] = toml_edit::value(format!("Qwen {bits}-bit (local)"));
            model["description"] = toml_edit::value(format!(
                "Local mlx-vlm {bits}-bit — /model qwen3-8-27b-{bits}bit"
            ));
            let plan = plan_for_quant(&model_root, bits);
            model["api_key"] = toml_edit::value(DUMMY_API_KEY);
            model["api_backend"] = toml_edit::value("chat_completions");
            model["context_window"] = toml_edit::value(plan.context_window as i64);
            model["max_completion_tokens"] = toml_edit::value(plan.max_completion_tokens as i64);
            model["inference_idle_timeout_secs"] = toml_edit::value(IDLE_TIMEOUT_SECS);
            model["supports_reasoning_effort"] = toml_edit::value(false);
            model["supports_backend_search"] = toml_edit::value(false);
            model["stream_tool_calls"] = toml_edit::value(false);
            model["use_concise"] = toml_edit::value(true);
            model["temperature"] = toml_edit::value(0.7);
        }
    }

    if let Some(bits) = default_bits {
        doc["models"]["default"] = toml_edit::value(model_id(bits));
    }

    fs::write(&config_path, doc.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600));
    }

    println!(
        "Wrote local Qwen model entries to {}",
        config_path.display()
    );
    for bits in SUPPORTED_BITS {
        let id = model_id(bits);
        let plan = plan_for_quant(&model_root, bits);
        print!(
            "  {id}  —  /model {id}  context={}  max_out={}",
            plan.context_window, plan.max_completion_tokens
        );
        if plan.tight {
            print!("  (tight RAM — expect swap)");
        }
        println!();
    }
    if let Some(bits) = default_bits {
        println!("Default model set to {}", model_id(bits));
    } else {
        println!(
            "Switch in the TUI with /model {}, or:",
            model_id(DEFAULT_BITS)
        );
        println!("  ds local use {DEFAULT_BITS}");
    }
    Ok(())
}

fn cmd_download(bits: Option<u8>) -> Result<()> {
    let bits_list = match bits {
        Some(b) => vec![parse_bits(b)?],
        None => SUPPORTED_BITS.to_vec(),
    };
    let python = require_venv_python()?;
    let root = default_model_root();
    fs::create_dir_all(&root)?;

    for b in bits_list {
        if quant_ready(&root, b) {
            println!(
                "{}-bit already present at {}",
                b,
                quant_dir(&root, b).display()
            );
            continue;
        }
        println!("Downloading {HF_REPO} {b}-bit → {}", root.display());
        let mut cmd = Command::new(&python);
        cmd.args([
            "-m",
            "huggingface_hub.cli.hf",
            "download",
            HF_REPO,
            "--include",
            &format!("{b}-bit/*"),
            "--local-dir",
            root.to_str().context("model dir is not UTF-8")?,
        ]);
        // Prefer `hf` if the venv shipped the console script.
        let status = if hf_bin().is_file() {
            Command::new(hf_bin())
                .args([
                    "download",
                    HF_REPO,
                    "--include",
                    &format!("{b}-bit/*"),
                    "--local-dir",
                    root.to_str().context("model dir is not UTF-8")?,
                ])
                .status()
                .context("hf download failed to start")?
        } else {
            cmd.status()
                .context("huggingface_hub download failed to start")?
        };
        if !status.success() {
            bail!(
                "download of {b}-bit failed (exit {status}). \
                 Accept the terms at https://huggingface.co/{HF_REPO} \
                 and set HF_TOKEN to a Hugging Face read token."
            );
        }
        if !quant_ready(&root, b) {
            bail!(
                "download finished but {} is missing config.json / safetensors",
                quant_dir(&root, b).display()
            );
        }
        println!("{}-bit ready", b);
    }
    Ok(())
}

fn server_args(
    model: &Path,
    host: &str,
    port: u16,
    plan: &LocalContextPlan,
) -> Result<Vec<String>> {
    Ok(vec![
        "-m".into(),
        "mlx_vlm".into(),
        "server".into(),
        "--model".into(),
        model
            .to_str()
            .context("model path is not UTF-8")?
            .to_string(),
        "--host".into(),
        host.to_string(),
        "--port".into(),
        port.to_string(),
        // HF card: do not enable KV-cache quantization on this VL architecture.
        "--max-tokens".into(),
        plan.max_completion_tokens.to_string(),
        "--max-kv-size".into(),
        plan.context_window.to_string(),
        "--thinking-budget".into(),
        plan.thinking_budget.to_string(),
        "--enable-thinking".into(),
    ])
}

fn require_venv_python() -> Result<PathBuf> {
    let python = venv_python();
    if python.is_file() {
        Ok(python)
    } else {
        bail!(
            "mlx-vlm venv not found at {}. Recreate it with:\n  \
             python3 -m venv {dir}/venv && {dir}/venv/bin/pip install -U 'mlx>=0.32' 'mlx-vlm>=0.6.13' 'huggingface_hub[cli,hf_xet]' jinja2",
            python.display(),
            dir = state_dir().display()
        )
    }
}

fn cmd_serve(bits: Option<u8>, port: u16, host: String, foreground: bool) -> Result<()> {
    let mut state = load_state();
    let bits = match bits {
        Some(b) => parse_bits(b)?,
        None => parse_bits(state.bits).unwrap_or(DEFAULT_BITS),
    };
    let root = PathBuf::from(&state.model_dir);
    let model = quant_dir(&root, bits);
    if !quant_ready(&root, bits) {
        bail!(
            "{} is not downloaded yet. Run: ds local download --bits {bits}",
            model.display()
        );
    }
    if let Some(pid) = read_owned_pid() {
        bail!("local mlx-vlm already running (pid {pid}). Stop it with: ds local stop");
    }

    let python = require_venv_python()?;
    fs::create_dir_all(state_dir())?;
    let plan = plan_for_quant(&root, bits);
    println!(
        "Context auto: {} tokens (model max {}, KV ~{:.1} GiB, weights {:.1} GiB, RAM {:.1} GiB)",
        plan.context_window,
        plan.model_max,
        (plan.context_window as f64 * plan.kv_bytes_per_token as f64) / 1024.0 / 1024.0 / 1024.0,
        plan.weight_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
        plan.ram_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
    );
    if plan.tight {
        println!("Warning: leftover RAM after weights is tight; this quant may swap.");
    }

    if foreground {
        state.bits = bits;
        state.host = host.clone();
        state.port = port;
        state.pid = None;
        save_state(&state)?;
        println!(
            "Serving {} on http://{host}:{port}/v1  (ctrl-c to stop)",
            model.display()
        );
        let status = Command::new(python)
            .args(server_args(&model, &host, port, &plan)?)
            .status()
            .context("failed to start mlx_vlm server")?;
        if !status.success() {
            bail!("mlx_vlm server exited with {status}");
        }
        return Ok(());
    }

    let log = fs::File::create(log_path()).context("create server.log")?;
    let mut cmd = Command::new(python);
    cmd.args(server_args(&model, &host, port, &plan)?)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn().context("failed to spawn mlx_vlm server")?;
    let pid = child.id();
    // Detach: drop Child without wait so the process keeps running.
    std::mem::forget(child);

    fs::write(pid_path(), format!("{pid}\n"))?;
    state.bits = bits;
    state.host = host.clone();
    state.port = port;
    state.pid = Some(pid);
    save_state(&state)?;

    println!("Started mlx-vlm pid {pid} ({bits}-bit) on http://{host}:{port}/v1");
    println!("Waiting for the server to accept connections (first load can take several minutes)…");
    if wait_for_health(&host, port, HEALTH_WAIT) {
        println!("Ready. Use: ds --model {}", model_id(bits));
    } else {
        println!(
            "Server has not answered yet. Tail {} and retry `ds local status`.",
            log_path().display()
        );
    }
    Ok(())
}

fn wait_for_health(host: &str, port: u16, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if http_models_ok(host, port) {
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}

fn http_models_ok(host: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let req =
        format!("GET /v1/models HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    buf.contains("200") || buf.contains("data")
}

fn cmd_stop() -> Result<()> {
    let Some(pid) = read_owned_pid() else {
        let _ = fs::remove_file(pid_path());
        println!("No local mlx-vlm server started by `ds local serve` is running.");
        return Ok(());
    };
    #[cfg(unix)]
    {
        let err = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if err != 0 {
            bail!(
                "failed to signal pid {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if !ds_shell::util::is_process_alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if ds_shell::util::is_process_alive(pid) {
            // Still the process we started this session via the pid file.
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
    let _ = fs::remove_file(pid_path());
    let mut state = load_state();
    state.pid = None;
    save_state(&state)?;
    println!("Stopped local mlx-vlm server (pid {pid}).");
    Ok(())
}

fn cmd_status(json: bool) -> Result<()> {
    let state = load_state();
    let root = PathBuf::from(&state.model_dir);
    let pid = read_owned_pid();
    let ready: Vec<u8> = SUPPORTED_BITS
        .into_iter()
        .filter(|b| quant_ready(&root, *b))
        .collect();
    let healthy = pid.is_some() && http_models_ok(&state.host, state.port);

    if json {
        let windows: serde_json::Map<String, serde_json::Value> = SUPPORTED_BITS
            .into_iter()
            .map(|b| {
                let p = plan_for_quant(&root, b);
                (
                    format!("{b}bit"),
                    serde_json::json!({
                        "context_window": p.context_window,
                        "max_completion_tokens": p.max_completion_tokens,
                        "tight": p.tight,
                    }),
                )
            })
            .collect();
        let payload = serde_json::json!({
            "repo": HF_REPO,
            "model_dir": root,
            "bits_ready": ready,
            "configured_bits": state.bits,
            "host": state.host,
            "port": state.port,
            "pid": pid,
            "healthy": healthy,
            "venv_python": venv_python(),
            "venv_ok": venv_python().is_file(),
            "ram_bytes": physical_ram_bytes(),
            "windows": windows,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("Local Qwen / mlx-vlm");
    println!("  repo:       {HF_REPO}");
    println!("  model dir:  {}", root.display());
    println!(
        "  venv:       {} ({})",
        venv_python().display(),
        if venv_python().is_file() {
            "ok"
        } else {
            "missing"
        }
    );
    for b in SUPPORTED_BITS {
        let mark = if quant_ready(&root, b) {
            "ready"
        } else {
            "not downloaded"
        };
        let plan = plan_for_quant(&root, b);
        println!(
            "  {b}-bit:      {mark}  context={}  max_out={}",
            plan.context_window, plan.max_completion_tokens
        );
    }
    match pid {
        Some(p) if healthy => println!(
            "  server:     up pid {p}  http://{}:{}/v1  ({}-bit)",
            state.host, state.port, state.bits
        ),
        Some(p) => println!(
            "  server:     pid {p} starting/unhealthy  (see {})",
            log_path().display()
        ),
        None => println!("  server:     down  (ds local serve)"),
    }
    Ok(())
}

fn cmd_use(bits: u8) -> Result<()> {
    let bits = parse_bits(bits)?;
    cmd_setup(Some(bits))?;
    let mut state = load_state();
    state.bits = bits;
    save_state(&state)?;
    if read_owned_pid().is_some() {
        println!("Restarting server on {bits}-bit…");
        cmd_stop()?;
        cmd_serve(Some(bits), state.port, state.host, false)?;
    } else {
        println!(
            "Default is now {}. Start the server with: ds local serve --bits {bits}",
            model_id(bits)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bits_accepts_usable_quants() {
        assert_eq!(parse_bits(4).unwrap(), 4);
        assert_eq!(parse_bits(6).unwrap(), 6);
        assert_eq!(parse_bits(8).unwrap(), 8);
        assert!(parse_bits(2).is_err());
        assert!(parse_bits(3).is_err());
    }

    #[test]
    fn model_ids_are_stable() {
        assert_eq!(model_id(4), "qwen3-8-27b-4bit");
        assert_eq!(model_id(8), "qwen3-8-27b-8bit");
    }

    #[test]
    fn quant_dir_nests_under_root() {
        let root = PathBuf::from("/tmp/qwen");
        assert_eq!(quant_dir(&root, 4), PathBuf::from("/tmp/qwen/4-bit"));
    }

    #[test]
    fn qwen38_kv_is_64kib_per_token() {
        let arch = ModelArch::qwen38_27b();
        assert_eq!(kv_bytes_per_token(&arch), 65_536);
    }

    #[test]
    fn parse_arch_counts_full_attention_layers() {
        let v = serde_json::json!({
            "text_config": {
                "max_position_embeddings": 262144,
                "num_hidden_layers": 64,
                "num_key_value_heads": 4,
                "head_dim": 256,
                "layer_types": [
                    "linear_attention",
                    "linear_attention",
                    "linear_attention",
                    "full_attention",
                    "linear_attention",
                    "full_attention"
                ]
            }
        });
        let arch = parse_model_arch(&v);
        assert_eq!(arch.full_attention_layers, 2);
        assert_eq!(arch.max_position_embeddings, 262_144);
    }

    #[test]
    fn context_on_32gib_fits_4bit_not_1m() {
        let arch = ModelArch::qwen38_27b();
        let ram = 32u64 << 30;
        let p4 = plan_context(ram, 15 << 30, &arch);
        let p6 = plan_context(ram, 22 << 30, &arch);
        let p8 = plan_context(ram, 28 << 30, &arch);
        assert_eq!(p4.context_window, 49_152);
        assert_eq!(p4.max_completion_tokens, 8_192);
        assert!(!p4.tight);
        assert_eq!(p6.context_window, 16_384);
        assert_eq!(p6.max_completion_tokens, 4_096);
        assert_eq!(p8.context_window, 4_096);
        assert!(p8.tight);
        assert!(p4.context_window < 200_000);
        assert!(p4.context_window < p4.model_max);
    }

    #[test]
    fn context_on_64gib_caps_at_local_max() {
        let arch = ModelArch::qwen38_27b();
        let p = plan_context(64 << 30, 15 << 30, &arch);
        assert_eq!(p.context_window, MAX_LOCAL_CONTEXT_WINDOW);
        assert!(!p.tight);
    }

    #[test]
    fn snap_window_picks_ladder_not_raw() {
        assert_eq!(snap_window(60_774, 65_536), 49_152);
        assert_eq!(snap_window(1_000, 65_536), 4_096);
        assert_eq!(snap_window(80_000, 65_536), 65_536);
    }

    #[test]
    fn server_args_pin_kv_and_completion() {
        let plan = LocalContextPlan {
            context_window: 16_384,
            max_completion_tokens: 4_096,
            thinking_budget: 1_024,
            kv_bytes_per_token: 65_536,
            weight_bytes: 1,
            ram_bytes: 1,
            model_max: 262_144,
            tight: false,
        };
        let args = server_args(Path::new("/tmp/4-bit"), "127.0.0.1", 8080, &plan).unwrap();
        let kv = args.iter().position(|a| a == "--max-kv-size").unwrap();
        assert_eq!(args[kv + 1], "16384");
        let mt = args.iter().position(|a| a == "--max-tokens").unwrap();
        assert_eq!(args[mt + 1], "4096");
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--thinking-budget" && w[1] == "1024"));
        assert!(!args.iter().any(|a| a.contains("kv-bits")));
    }
}
