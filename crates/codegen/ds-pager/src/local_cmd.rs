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

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

const HF_REPO: &str = "orcarouter/Qwen3.8-27B-Uncensored-MLX";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_BITS: u8 = 4;
const SUPPORTED_BITS: [u8; 3] = [4, 6, 8];
const DUMMY_API_KEY: &str = "local";
const CONTEXT_WINDOW: i64 = 32_768;
const MAX_COMPLETION_TOKENS: i64 = 8_192;
const IDLE_TIMEOUT_SECS: i64 = 600;
const HEALTH_WAIT: Duration = Duration::from_secs(300);

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
            model["name"] =
                toml_edit::value(format!("Qwen3.8-27B Uncensored {bits}-bit (local MLX)"));
            model["description"] =
                toml_edit::value(format!("Local mlx-vlm server — {HF_REPO} {bits}-bit"));
            model["api_key"] = toml_edit::value(DUMMY_API_KEY);
            model["api_backend"] = toml_edit::value("chat_completions");
            model["context_window"] = toml_edit::value(CONTEXT_WINDOW);
            model["max_completion_tokens"] = toml_edit::value(MAX_COMPLETION_TOKENS);
            model["inference_idle_timeout_secs"] = toml_edit::value(IDLE_TIMEOUT_SECS);
            model["supports_reasoning_effort"] = toml_edit::value(false);
            model["supports_backend_search"] = toml_edit::value(false);
            model["stream_tool_calls"] = toml_edit::value(false);
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
        println!("  {}  —  ds --model {}", model_id(bits), model_id(bits));
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

fn server_args(model: &Path, host: &str, port: u16) -> Result<Vec<String>> {
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
        MAX_COMPLETION_TOKENS.to_string(),
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
            .args(server_args(&model, &host, port)?)
            .status()
            .context("failed to start mlx_vlm server")?;
        if !status.success() {
            bail!("mlx_vlm server exited with {status}");
        }
        return Ok(());
    }

    let log = fs::File::create(log_path()).context("create server.log")?;
    let mut cmd = Command::new(python);
    cmd.args(server_args(&model, &host, port)?)
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
        println!("  {b}-bit:      {mark}");
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
}
