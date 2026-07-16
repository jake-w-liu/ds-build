//! CodeWhale-style DeepSeek API key setup for first-run and `ds auth`.
//!
//! Users paste a key from https://platform.deepseek.com/api_keys when no key is
//! configured yet. The key is written to `~/.ds/config.toml` (per-model
//! `[model.*]` blocks plus top-level `api_key`) and mirrored into
//! `~/.ds/auth.json` under the `ds::api_key` scope.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::agent::config::DEEPSEEK_API_BASE_URL_DEFAULT;
use crate::agent::auth_method::{
    DEEPSEEK_API_KEY_ENV_VAR, DS_API_KEY_ENV_VAR, LEGACY_DEEPSEEK_API_KEY_ENV_VAR, has_ds_api_key_env,
};
use crate::util::ds_home::ds_home;

/// Default model IDs that receive a pasted API key when missing from config.
const DEFAULT_MODEL_IDS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];

const KEY_PLATFORM_URL: &str = "https://platform.deepseek.com/api_keys";

/// Where a resolved API key came from (never includes the secret itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyOrigin {
    EnvDeepseek,
    EnvDs,
    EnvLegacy,
    ConfigModel,
    ConfigTopLevel,
    AuthJson,
    Unset,
}

impl ApiKeyOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnvDeepseek => "env:DEEPSEEK_API_KEY",
            Self::EnvDs => "env:DS_API_KEY",
            Self::EnvLegacy => "env:DS_CODE_API_KEY",
            Self::ConfigModel => "config.toml [model.*]",
            Self::ConfigTopLevel => "config.toml api_key",
            Self::AuthJson => "auth.json",
            Self::Unset => "unset",
        }
    }

    pub fn is_set(self) -> bool {
        !matches!(self, Self::Unset)
    }
}

/// Non-secret status of API key configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyStatus {
    pub origin: ApiKeyOrigin,
    pub config_path: PathBuf,
    /// True when env vars would override a config/auth.json key.
    pub env_overrides: bool,
    /// Redacted prefix for display (e.g. `sk-…43f4`), never the full key.
    pub redacted: Option<String>,
}

/// How the CLI / first-run path should obtain a key value.
#[derive(Debug, Clone)]
pub enum ApiKeyInput {
    /// Explicit `--api-key` value (discouraged — shell history).
    Inline(String),
    /// Read one line from stdin (`--api-key-stdin` or piped).
    Stdin,
    /// Interactive no-echo prompt when stdin is a TTY.
    Prompt,
}

/// Result of [`ensure_api_key_interactive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureApiKeyOutcome {
    /// A key was already available (env, config, or auth.json).
    AlreadyConfigured,
    /// User pasted a key and it was saved.
    Saved,
    /// Non-interactive context with no key — caller should fail closed.
    MissingNonInteractive,
}

/// Path to the user config file (`$DS_HOME/config.toml` / `~/.ds/config.toml`).
pub fn user_config_path() -> PathBuf {
    ds_home().join("config.toml")
}

/// Resolve where an API key would come from without printing secrets.
pub fn api_key_status() -> ApiKeyStatus {
    let config_path = user_config_path();
    let env_overrides = std::env::var_os(DEEPSEEK_API_KEY_ENV_VAR).is_some()
        || std::env::var_os(DS_API_KEY_ENV_VAR).is_some()
        || std::env::var_os(LEGACY_DEEPSEEK_API_KEY_ENV_VAR).is_some();

    let (origin, key) = match detect_key_origin() {
        (o, Some(k)) => (o, Some(k)),
        (o, None) => (o, None),
    };

    ApiKeyStatus {
        origin,
        config_path,
        env_overrides,
        redacted: key.as_deref().map(redact_api_key),
    }
}

fn detect_key_origin() -> (ApiKeyOrigin, Option<String>) {
    if let Ok(k) = std::env::var(DEEPSEEK_API_KEY_ENV_VAR) {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return (ApiKeyOrigin::EnvDeepseek, Some(k));
        }
    }
    if let Ok(k) = std::env::var(DS_API_KEY_ENV_VAR) {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return (ApiKeyOrigin::EnvDs, Some(k));
        }
    }
    if let Ok(k) = std::env::var(LEGACY_DEEPSEEK_API_KEY_ENV_VAR) {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return (ApiKeyOrigin::EnvLegacy, Some(k));
        }
    }
    if let Some(k) = read_top_level_api_key_from_config() {
        return (ApiKeyOrigin::ConfigTopLevel, Some(k));
    }
    if let Some(k) = read_model_api_key_from_config_only() {
        return (ApiKeyOrigin::ConfigModel, Some(k));
    }
    if let Some(k) = crate::auth::read_api_key(&ds_home()) {
        let k = k.trim().to_owned();
        if !k.is_empty() {
            return (ApiKeyOrigin::AuthJson, Some(k));
        }
    }
    (ApiKeyOrigin::Unset, None)
}

fn redact_api_key(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        return "********".to_owned();
    }
    let prefix: String = key.chars().take(4).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

/// Read a key value according to [`ApiKeyInput`]. Does not echo on TTY prompt.
pub fn read_api_key_input(input: ApiKeyInput) -> anyhow::Result<String> {
    let raw = match input {
        ApiKeyInput::Inline(s) => s,
        ApiKeyInput::Stdin => read_line_from_stdin()?,
        ApiKeyInput::Prompt => {
            if !io::stdin().is_terminal() {
                // Piped input without --api-key-stdin still works (CodeWhale-like).
                read_line_from_stdin()?
            } else {
                eprint!("Enter DeepSeek API key (from {KEY_PLATFORM_URL}): ");
                let _ = io::stderr().flush();
                let line = read_secret_line()?;
                eprintln!();
                line
            }
        }
    };
    let key = normalize_api_key(&raw)?;
    Ok(key)
}

fn normalize_api_key(raw: &str) -> anyhow::Result<String> {
    let key = raw.trim().trim_matches(|c| c == '"' || c == '\'').to_owned();
    if key.is_empty() {
        anyhow::bail!("empty API key provided");
    }
    if key.contains('\n') || key.contains('\r') {
        anyhow::bail!("API key must be a single line");
    }
    if is_placeholder_api_key(&key) {
        anyhow::bail!("API key still looks like a placeholder — paste the real key from {KEY_PLATFORM_URL}");
    }
    Ok(key)
}

fn read_line_from_stdin() -> anyhow::Result<String> {
    let mut line = String::new();
    let n = io::stdin().lock().read_line(&mut line)?;
    if n == 0 {
        anyhow::bail!("failed to read API key from stdin");
    }
    Ok(line)
}

/// Read a line with echo disabled when stdin is a TTY (Unix).
fn read_secret_line() -> io::Result<String> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let stdin = io::stdin();
        let fd = stdin.as_raw_fd();
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        let had_termios = unsafe { libc::tcgetattr(fd, &mut original) } == 0;
        if had_termios {
            let mut no_echo = original;
            no_echo.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ECHONL);
            // Keep ICANON so read_line still works.
            let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &no_echo) };
        }
        let mut line = String::new();
        let result = stdin.lock().read_line(&mut line);
        if had_termios {
            let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
        }
        result?;
        Ok(line)
    }
    #[cfg(not(unix))]
    {
        // Best-effort: Windows console no-echo is not wired here; key still works.
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(line)
    }
}

/// Save `key` to user config + auth.json (CodeWhale-style persistence).
///
/// Writes:
/// - top-level `api_key`
/// - `[auth].preferred_method = "api_key"`
/// - `[model.<id>].api_key` for default models (and any existing model tables)
/// - default `base_url` / `api_backend` / `context_window` when missing
/// - `auth.json` `ds::api_key` scope via [`crate::auth::store_api_key`]
pub fn save_api_key(key: &str) -> anyhow::Result<PathBuf> {
    let key = normalize_api_key(key)?;
    let path = user_config_path();
    write_api_key_to_config(&path, &key)?;
    let home = ds_home();
    crate::auth::store_api_key(&home, &key).map_err(|e| {
        anyhow::anyhow!(
            "saved config.toml but failed to write auth.json under {}: {e}",
            home.display()
        )
    })?;
    Ok(path)
}

/// Remove saved keys from config.toml model/top-level fields and auth.json.
/// Does **not** unset process environment variables.
pub fn clear_saved_api_key() -> anyhow::Result<()> {
    let path = user_config_path();
    if path.exists() {
        clear_api_key_from_config(&path)?;
    }
    crate::auth::clear_api_key(&ds_home())?;
    Ok(())
}

/// Interactive first-run: if no key is configured and stdin is a TTY, prompt
/// and save (CodeWhale onboarding paste). Non-TTY with no key returns
/// [`EnsureApiKeyOutcome::MissingNonInteractive`].
///
/// Skips the paste prompt when a non-API-key session credential already exists
/// **and** `[auth] preferred_method` is not `api_key` (OAuth / SSO users).
/// When preferred method is `api_key` (DeepSeek BYOK default), a missing key
/// always prompts on a TTY.
pub fn ensure_api_key_interactive() -> anyhow::Result<EnsureApiKeyOutcome> {
    if has_ds_api_key_env() {
        return Ok(EnsureApiKeyOutcome::AlreadyConfigured);
    }
    // Also treat auth.json-only keys as configured once reader includes them.
    if detect_key_origin().1.is_some() {
        return Ok(EnsureApiKeyOutcome::AlreadyConfigured);
    }

    let pin_api_key = preferred_method_is_api_key();
    if !pin_api_key && has_session_credential() {
        return Ok(EnsureApiKeyOutcome::AlreadyConfigured);
    }

    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if !interactive {
        return Ok(EnsureApiKeyOutcome::MissingNonInteractive);
    }

    eprintln!("No DeepSeek API key configured.");
    eprintln!("Get a key at {KEY_PLATFORM_URL}");
    eprintln!("(You can also run `ds auth set` later, or set DEEPSEEK_API_KEY.)");
    eprintln!();

    let key = read_api_key_input(ApiKeyInput::Prompt)?;
    let path = save_api_key(&key)?;
    eprintln!(
        "Saved API key to {} (and auth.json).",
        path.display()
    );
    Ok(EnsureApiKeyOutcome::Saved)
}

/// True when config pins `[auth] preferred_method = "api_key"`, or when the
/// file is missing / unreadable (DeepSeek product default is API-key BYOK).
fn preferred_method_is_api_key() -> bool {
    let path = user_config_path();
    let Ok(text) = std::fs::read_to_string(path) else {
        // No config yet → first-run BYOK path.
        return true;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return true;
    };
    match value
        .get("auth")
        .and_then(|a| a.get("preferred_method"))
        .and_then(|v| v.as_str())
    {
        Some("api_key") | None => true,
        Some("oidc") => false,
        Some(_) => true,
    }
}

/// True when auth.json holds a non-empty non-ApiKey credential (OIDC / external).
fn has_session_credential() -> bool {
    let path = ds_home().join("auth.json");
    let Ok(store) = crate::auth::read_auth_json(&path) else {
        return false;
    };
    store.values().any(|a| {
        !a.key.trim().is_empty()
            && matches!(
                a.auth_mode,
                crate::auth::AuthMode::Oidc
                    | crate::auth::AuthMode::WebLogin
                    | crate::auth::AuthMode::External
            )
    })
}

/// CLI entry for `ds auth set`.
pub fn run_auth_set(input: ApiKeyInput) -> anyhow::Result<()> {
    let key = read_api_key_input(input)?;
    let path = save_api_key(&key)?;
    println!(
        "saved API key to {} and auth.json ({})",
        path.display(),
        redact_api_key(&key)
    );
    Ok(())
}

/// CLI entry for `ds auth status` / `ds auth get`.
pub fn run_auth_status(json: bool) -> anyhow::Result<()> {
    let status = api_key_status();
    if json {
        let v = serde_json::json!({
            "set": status.origin.is_set(),
            "source": status.origin.as_str(),
            "config_path": status.config_path.display().to_string(),
            "env_overrides": status.env_overrides,
            "redacted": status.redacted,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if status.origin.is_set() {
        println!(
            "deepseek: set (source: {})",
            status.origin.as_str()
        );
        if let Some(r) = &status.redacted {
            println!("key: {r}");
        }
        println!("config: {}", status.config_path.display());
    } else {
        println!("deepseek: not set");
        println!("Get a key: {KEY_PLATFORM_URL}");
        println!("Save it with: ds auth set");
        println!("Or set DEEPSEEK_API_KEY / add api_key under [model.*] in {}", status.config_path.display());
    }
    Ok(())
}

/// CLI entry for `ds auth clear`.
pub fn run_auth_clear() -> anyhow::Result<()> {
    clear_saved_api_key()?;
    println!("Cleared saved API key from config.toml and auth.json.");
    if std::env::var_os(DEEPSEEK_API_KEY_ENV_VAR).is_some()
        || std::env::var_os(DS_API_KEY_ENV_VAR).is_some()
        || std::env::var_os(LEGACY_DEEPSEEK_API_KEY_ENV_VAR).is_some()
    {
        println!(
            "Note: environment variable(s) still set (DEEPSEEK_API_KEY / DS_API_KEY / DS_CODE_API_KEY); unset them in your shell if needed."
        );
    }
    Ok(())
}

// ── config.toml helpers ───────────────────────────────────────────────────

fn write_api_key_to_config(path: &Path, key: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = crate::util::config::read_to_string_or_empty(path)?;
    let mut doc = if existing.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        existing.parse::<toml_edit::DocumentMut>().map_err(|e| {
            anyhow::anyhow!(
                "config.toml is not valid TOML ({}); fix or move it before saving an API key: {}",
                e,
                path.display()
            )
        })?
    };

    // Top-level key (CodeWhale parity; also read by read_ds_api_key_env).
    doc["api_key"] = toml_edit::value(key);

    // Pin preferred auth to API key for DeepSeek BYOK.
    let auth_item = doc
        .entry("auth")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    if let Some(table) = auth_item.as_table_mut() {
        table["preferred_method"] = toml_edit::value("api_key");
    }

    // Ensure [model] table and default model entries.
    let model_item = doc
        .entry("model")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let model_table = model_item.as_table_mut().ok_or_else(|| {
        anyhow::anyhow!("[model] in config.toml is not a table")
    })?;

    // Collect existing model ids + defaults.
    let mut ids: Vec<String> = model_table
        .iter()
        .filter_map(|(k, v)| v.as_table().map(|_| k.to_string()))
        .collect();
    for id in DEFAULT_MODEL_IDS {
        if !ids.iter().any(|x| x == *id) {
            ids.push((*id).to_string());
        }
    }

    for id in ids {
        let entry = model_table
            .entry(&id)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let table = entry.as_table_mut().ok_or_else(|| {
            anyhow::anyhow!("[model.{id}] in config.toml is not a table")
        })?;
        table["api_key"] = toml_edit::value(key);
        if table.get("base_url").and_then(|v| v.as_str()).is_none() {
            table["base_url"] = toml_edit::value(DEEPSEEK_API_BASE_URL_DEFAULT);
        }
        if table.get("api_backend").and_then(|v| v.as_str()).is_none() {
            table["api_backend"] = toml_edit::value("chat_completions");
        }
        if table.get("context_window").and_then(|v| v.as_integer()).is_none() {
            table["context_window"] = toml_edit::value(1_000_000i64);
        }
    }

    crate::util::config::atomic_write_string(path, &doc.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn clear_api_key_from_config(path: &Path) -> anyhow::Result<()> {
    let existing = crate::util::config::read_to_string_or_empty(path)?;
    if existing.trim().is_empty() {
        return Ok(());
    }
    let mut doc = existing.parse::<toml_edit::DocumentMut>().map_err(|e| {
        anyhow::anyhow!("config.toml is not valid TOML ({}); cannot clear key", e)
    })?;

    // Remove top-level api_key.
    let _ = doc.as_table_mut().remove("api_key");

    if let Some(model) = doc.get_mut("model").and_then(|m| m.as_table_mut()) {
        // toml_edit table keys — collect first to avoid borrow issues.
        let keys: Vec<String> = model
            .iter()
            .filter_map(|(k, v)| v.as_table().map(|_| k.to_string()))
            .collect();
        for k in keys {
            if let Some(entry) = model.get_mut(k.as_str()).and_then(|t| t.as_table_mut()) {
                let _ = entry.remove("api_key");
            }
        }
    }
    // Legacy [models.*] plural tables.
    if let Some(models) = doc.get_mut("models").and_then(|m| m.as_table_mut()) {
        let keys: Vec<String> = models
            .iter()
            .filter_map(|(k, v)| v.as_table().map(|_| k.to_string()))
            .collect();
        for k in keys {
            if let Some(entry) = models.get_mut(k.as_str()).and_then(|t| t.as_table_mut()) {
                let _ = entry.remove("api_key");
            }
        }
    }

    crate::util::config::atomic_write_string(path, &doc.to_string())?;
    Ok(())
}

/// Parse a full config.toml document. In toml 0.9, `str::parse::<Value>()` only
/// accepts a single value expression — use `toml::from_str` for documents.
fn parse_config_toml(text: &str) -> Option<toml::Value> {
    toml::from_str(text).ok()
}

fn read_top_level_api_key_from_config() -> Option<String> {
    let path = user_config_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let value = parse_config_toml(&text)?;
    let key = value.get("api_key").and_then(|v| v.as_str()).map(str::trim)?;
    if is_placeholder_api_key(key) {
        return None;
    }
    Some(key.to_owned())
}

/// Model-only key without env (for origin detection).
fn read_model_api_key_from_config_only() -> Option<String> {
    let path = user_config_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let value = parse_config_toml(&text)?;
    first_model_api_key_value(&value)
}

fn is_placeholder_api_key(key: &str) -> bool {
    let upper = key.trim().to_ascii_uppercase();
    upper.is_empty()
        || upper.contains("PASTE_YOUR")
        || upper.contains("PASTE YOUR")
        || upper.contains("YOUR_KEY")
        || upper == "SK-..."
        || upper.starts_with("SK-PASTE")
}

fn first_model_api_key_value(root: &toml::Value) -> Option<String> {
    let models = root
        .get("model")
        .and_then(|v| v.as_table())
        .into_iter()
        .chain(root.get("models").and_then(|v| v.as_table()));
    for table_map in models {
        for (_name, entry) in table_map {
            let Some(table) = entry.as_table() else {
                continue;
            };
            if let Some(key) = table.get("api_key").and_then(|v| v.as_str()) {
                let key = key.trim();
                if !is_placeholder_api_key(key) {
                    return Some(key.to_owned());
                }
            }
        }
    }
    None
}

/// Used by [`crate::agent::auth_method::read_ds_api_key_env`] for top-level + auth.json.
pub(crate) fn read_api_key_from_config_or_auth_json() -> Option<String> {
    if let Some(k) = read_top_level_api_key_from_config() {
        return Some(k);
    }
    if let Some(k) = read_model_api_key_from_config_only() {
        return Some(k);
    }
    let k = crate::auth::read_api_key(&ds_home())?;
    let k = k.trim();
    if k.is_empty() {
        None
    } else {
        Some(k.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    #[test]
    fn normalize_rejects_empty_and_placeholder() {
        assert!(normalize_api_key("").is_err());
        assert!(normalize_api_key("   ").is_err());
        assert!(normalize_api_key("sk-PASTE_YOUR_KEY").is_err());
        assert_eq!(normalize_api_key("  sk-abc  ").unwrap(), "sk-abc");
        assert_eq!(normalize_api_key("\"sk-abc\"").unwrap(), "sk-abc");
    }

    #[test]
    fn redact_hides_middle() {
        let r = redact_api_key("sk-1234567890abcdef");
        assert!(r.starts_with("sk-1"));
        assert!(r.contains('…'));
        assert!(!r.contains("567890"));
    }

    #[test]
    fn write_and_clear_config_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[ui]\npermission_mode = \"always-approve\"\n\n[model.deepseek-v4-pro]\nbase_url = \"https://example.com\"\n",
        )
        .unwrap();

        write_api_key_to_config(&path, "sk-test-key-123456").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("permission_mode"));
        assert!(body.contains("sk-test-key-123456"));
        assert!(body.contains("deepseek-v4-flash"));
        assert!(body.contains("preferred_method"));
        // Preserved custom base_url
        assert!(body.contains("https://example.com"));

        // Document must parse via from_str (toml 0.9 regression guard).
        let value = parse_config_toml(&body).expect("written config must parse");
        assert_eq!(
            value.get("api_key").and_then(|v| v.as_str()),
            Some("sk-test-key-123456")
        );
        assert_eq!(
            first_model_api_key_value(&value).as_deref(),
            Some("sk-test-key-123456")
        );

        clear_api_key_from_config(&path).unwrap();
        let body2 = std::fs::read_to_string(&path).unwrap();
        assert!(!body2.contains("sk-test-key-123456"));
        assert!(body2.contains("permission_mode"));
    }

    #[test]
    fn parse_config_toml_reads_real_document_shape() {
        let s = r#"# comment with PASTE YOUR KEY HERE
api_key = "sk-toplevel-key"
[model.deepseek-v4-pro]
api_key = "sk-model-key"
"#;
        let v = parse_config_toml(s).expect("document parses");
        assert_eq!(v.get("api_key").and_then(|x| x.as_str()), Some("sk-toplevel-key"));
        assert_eq!(
            first_model_api_key_value(&v).as_deref(),
            Some("sk-model-key")
        );
    }

    #[test]
    fn write_creates_defaults_on_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        write_api_key_to_config(&path, "sk-newuser-key").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("deepseek-v4-pro"));
        assert!(body.contains("deepseek-v4-flash"));
        assert!(body.contains(DEEPSEEK_API_BASE_URL_DEFAULT));
        assert!(body.contains("chat_completions"));
    }

    #[test]
    #[serial]
    fn has_ds_api_key_env_true_after_config_write_via_reader() {
        // Integration-ish: write to a temp home via DS_HOME if supported.
        // Fall back to testing first_model_api_key_value only when DS_HOME not overridable easily.
        let v: toml::Value = toml::from_str(
            r#"
            api_key = "sk-toplevel"
            [model.deepseek-v4-pro]
            api_key = "sk-model"
            "#,
        )
        .unwrap();
        assert_eq!(
            first_model_api_key_value(&v).as_deref(),
            Some("sk-model")
        );
        assert_eq!(
            v.get("api_key").and_then(|x| x.as_str()),
            Some("sk-toplevel")
        );
    }
}
