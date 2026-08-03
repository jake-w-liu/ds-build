//! Temp session homes + real `ds` process execution.
//!
//! Every scenario runs in an isolated `$HOME` (config + auth + sessions),
//! pointed at the recording server. The auth flow uses the SHIPPED
//! `ds auth set --api-key-stdin` command so credential handling matches
//! production; the scenario config is then written (same key).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use tempfile::TempDir;

use crate::usage::DsOutput;

/// What a scenario needs from the session shell environment.
#[derive(Debug, Clone)]
pub struct HomeConfig {
    pub api_key: String,
    /// Model traffic endpoint (recording server or live upstream).
    pub base_url: String,
    /// Settings endpoint (recording server in mock mode keeps it hermetic).
    pub ds_api_base_url: String,
    pub model: String,
    pub context_window: u64,
    pub subagents: bool,
    pub headroom: Option<bool>,
}

pub struct SessionHome {
    pub dir: TempDir,
    pub home: PathBuf,
    pub config_path: PathBuf,
}

impl SessionHome {
    /// Create the isolated HOME and authenticate it via the shipped CLI.
    pub fn setup(ds_bin: &Path, cfg: &HomeConfig) -> Result<Self> {
        let dir = tempfile::tempdir().context("temp home dir")?;
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".ds")).context("create ~/.ds")?;

        // Ship auth via the real command (writes config.toml + auth.json).
        let mut child = Command::new(ds_bin)
            .env("HOME", &home)
            .env("NO_COLOR", "1")
            .args(["auth", "set", "--api-key-stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn ds auth set")?;
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(cfg.api_key.as_bytes())?;
            }
        }
        let out = child.wait_with_output().context("wait ds auth set")?;
        anyhow::ensure!(
            out.status.success(),
            "ds auth set failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Overwrite config.toml with the scenario config (same key).
        let config_path = home.join(".ds").join("config.toml");
        let mut config = format!(
            r#"[auth]
preferred_method = "api_key"

[ui]
permission_mode = "always-approve"
fork_secondary_model = "deepseek-v4-pro"

[subagents]
enabled = {}

[models]
default_reasoning_effort = "max"
default = "{}"

[endpoints]
ds_api_base_url = "{}"

[model.{}]
api_key = "{}"
base_url = "{}"
api_backend = "chat_completions"
context_window = {}
supports_reasoning_effort = true
reasoning_effort = "max"
reasoning_efforts = ["high", "max"]

[features]
telemetry = false
"#,
            cfg.subagents,
            cfg.model,
            cfg.ds_api_base_url,
            cfg.model,
            cfg.api_key,
            cfg.base_url,
            cfg.context_window,
        );
        // Headroom OFF is expressed through the env var at spawn time; keep
        // config free of headroom toggles so `DS_HEADROOM` governs.
        let _ = &mut config;
        std::fs::write(&config_path, config).context("write scenario config")?;

        Ok(Self {
            dir,
            home,
            config_path,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }
}

/// Result of one `ds -p … --output-format json` invocation.
#[derive(Debug, Clone)]
pub struct InvocationResult {
    pub output: DsOutput,
    pub exit_code: i32,
    pub stdout: String,
    pub debug_log: PathBuf,
}

/// Run one headless `ds` invocation in the session home.
///
/// `first_turn` uses `--session-id`; subsequent turns use `--resume` so the
/// session accumulates real turns across processes.
#[allow(clippy::too_many_arguments)]
pub fn run_ds(
    ds_bin: &Path,
    home: &SessionHome,
    cwd: &Path,
    prompt: &str,
    session_id: &str,
    first_turn: bool,
    max_turns: u32,
    debug_log: &Path,
    headroom: Option<bool>,
    subagents: bool,
    web_search: bool,
) -> Result<InvocationResult> {
    let mut cmd = Command::new(ds_bin);
    cmd.env("HOME", home.home())
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "info,ds_headroom=debug")
        .current_dir(cwd);
    if let Some(on) = headroom {
        cmd.env("DS_HEADROOM", if on { "1" } else { "0" });
    } else {
        cmd.env_remove("DS_HEADROOM");
    }
    cmd.arg("-p").arg(prompt);
    cmd.arg("--output-format").arg("json");
    cmd.arg("--debug-file").arg(debug_log);
    if first_turn {
        cmd.arg("--session-id").arg(session_id);
    } else {
        cmd.arg("--resume").arg(session_id);
    }
    cmd.arg("--cwd").arg(cwd);
    cmd.arg("--max-turns").arg(max_turns.to_string());
    cmd.arg("--no-memory");
    cmd.arg("--always-approve");
    if !subagents {
        cmd.arg("--no-subagents");
    }
    if !web_search {
        cmd.arg("--disable-web-search");
    }

    let out = cmd
        .output()
        .with_context(|| format!("run ds ({prompt:?})"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let output: DsOutput = serde_json::from_str(&stdout).unwrap_or(DsOutput {
        text: None,
        stop_reason: None,
        session_id: None,
        usage: None,
        num_turns: None,
        kind: Some("error".into()),
        message: Some(format!(
            "unparseable ds output; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )),
    });
    Ok(InvocationResult {
        output,
        exit_code: out.status.code().unwrap_or(-1),
        stdout,
        debug_log: debug_log.to_path_buf(),
    })
}

/// Read the API key for live mode: `DEEPSEEK_API_KEY`, then `DS_API_KEY`,
/// then the key configured in `~/.ds/config.toml`. Never committed.
pub fn resolve_live_api_key() -> Result<String> {
    for var in ["DEEPSEEK_API_KEY", "DS_API_KEY"] {
        if let Ok(k) = std::env::var(var) {
            if !k.trim().is_empty() {
                return Ok(k.trim().to_string());
            }
        }
    }
    // Fall back to the user's existing config (same file `ds` reads).
    let cfg_path = home_config_path();
    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        if let Some(key) = extract_toml_api_key(&text) {
            return Ok(key);
        }
    }
    anyhow::bail!(
        "no API key: set DEEPSEEK_API_KEY (or DS_API_KEY), or add api_key to {}",
        cfg_path.display()
    )
}

fn home_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ds").join("config.toml")
}

/// Minimal TOML scan for `api_key = "…"` (any [model.*] section).
fn extract_toml_api_key(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("api_key") {
            let v = v.trim_start();
            let v = v.strip_prefix('=')?.trim_start();
            let v = v.trim_matches('"');
            if v.starts_with("sk-") && v.len() > 10 {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Resolve the `ds` binary: `DS_BIN` env override, else `ds` on PATH.
pub fn resolve_ds_bin() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("DS_BIN") {
        let p = PathBuf::from(p);
        anyhow::ensure!(p.exists(), "DS_BIN path does not exist: {}", p.display());
        return Ok(p);
    }
    let found = std::process::Command::new("which")
        .arg("ds")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    match found {
        Some(p) => Ok(PathBuf::from(p)),
        None => anyhow::bail!(
            "ds binary not found: set DS_BIN=/path/to/ds or ensure `ds` is on PATH"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_key_from_real_config_shape() {
        // Deliberately fake key — never put a real credential in the repo.
        let text = r#"[auth]
preferred_method = "api_key"

[model.deepseek-v4-flash]
api_key = "sk-test-fixture-key-0000000000000000000000"
base_url = "https://api.deepseek.com/v1"
"#;
        assert_eq!(
            extract_toml_api_key(text).as_deref(),
            Some("sk-test-fixture-key-0000000000000000000000")
        );
    }

    #[test]
    fn rejects_non_key_values() {
        assert!(extract_toml_api_key("api_key = \"dummy\"").is_none());
        assert!(extract_toml_api_key("").is_none());
    }
}
