//! Environment variable helpers and process isolation for terminal execution.
//!
//! All implementations now live in the lightweight [`ds_tty_utils`] crate
//! so that every crate in the workspace can use them without pulling in the
//! heavyweight `ds-tools` dependency. This module re-exports the public
//! API for backward compatibility.

pub use ds_tty_utils::{detach_from_tty, pager_env};

/// Env var set on agent-spawned terminal processes so host tools (e.g. `x ban`)
/// can distinguish agent invocations from human interactive shells.
/// Note: the CLI also uses `DS_AGENT` as an
/// optional agent-definition selector for launching `ds` itself; child terminal
/// processes only need the sentinel value `"1"`.
pub const DS_AGENT_ENV: &str = "DS_AGENT";

/// Sentinel value for [`DS_AGENT_ENV`] on agent tool terminals.
pub const DS_AGENT_ENV_VALUE: &str = "1";

/// Force `DS_AGENT=1` on an agent terminal child so request/login env cannot
/// clear the agent marker.
pub fn apply_ds_agent_marker(cmd: &mut tokio::process::Command) {
    cmd.env(DS_AGENT_ENV, DS_AGENT_ENV_VALUE);
}

/// Environment variable names that must never be inherited by agent-spawned
/// shell children (tools, sandboxed commands). Mirrors dscode's
/// `stripModelCredentialEnvironment` for DeepSeek-first local control.
pub const MODEL_CREDENTIAL_ENV_KEYS: &[&str] = &[
    "DEEPSEEK_API_KEY",
    "DS_CODE_API_KEY", // legacy DeepSeek key name
    "DS_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENROUTER_API_KEY",
    "ZAI_API_KEY",
    "KIMI_API_KEY",
    "MINIMAX_API_KEY",
    "XAI_API_KEY",
    "GROQ_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Remove model credential env vars from a child process command.
///
/// Call after request/login env is applied so a malicious or accidental
/// `env` map cannot re-inject keys. Parent process retains keys for the LLM
/// client.
pub fn scrub_model_credential_env(cmd: &mut tokio::process::Command) {
    for key in MODEL_CREDENTIAL_ENV_KEYS {
        cmd.env_remove(key);
    }
}

/// Drop model credential keys from an env map (request env, login env, etc.).
pub fn strip_model_credentials_from_map(env: &mut std::collections::HashMap<String, String>) {
    for key in MODEL_CREDENTIAL_ENV_KEYS {
        env.remove(*key);
        // Case-insensitive sweep for common variants.
        let upper = key.to_ascii_uppercase();
        env.retain(|k, _| k.to_ascii_uppercase() != upper);
    }
}

/// Expand the four plugin-path tokens (`${CLAUDE_PLUGIN_ROOT}` / `${DS_PLUGIN_ROOT}`
/// and `${CLAUDE_PLUGIN_DATA}` / `${DS_PLUGIN_DATA}`) in `s`. Each pair is expanded
/// only when its value is provided. Single source of truth for plugin agent bodies,
/// plugin skill/command bodies, and plugin MCP/hook config substitution.
pub fn substitute_plugin_tokens(
    s: &str,
    plugin_root: Option<&str>,
    plugin_data: Option<&str>,
) -> String {
    let mut out = s.to_string();
    if let Some(root) = plugin_root {
        out = out
            .replace("${CLAUDE_PLUGIN_ROOT}", root)
            .replace("${DS_PLUGIN_ROOT}", root);
    }
    if let Some(data) = plugin_data {
        out = out
            .replace("${CLAUDE_PLUGIN_DATA}", data)
            .replace("${DS_PLUGIN_DATA}", data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        DS_AGENT_ENV, DS_AGENT_ENV_VALUE, MODEL_CREDENTIAL_ENV_KEYS, strip_model_credentials_from_map,
        substitute_plugin_tokens,
    };
    use std::collections::HashMap;

    const ALL_TOKENS: &str = "${CLAUDE_PLUGIN_ROOT}/a ${DS_PLUGIN_ROOT}/b ${CLAUDE_PLUGIN_DATA}/c ${DS_PLUGIN_DATA}/d";

    #[test]
    fn expands_all_four_tokens_when_both_provided() {
        let out = substitute_plugin_tokens(ALL_TOKENS, Some("/root"), Some("/data"));
        assert_eq!(out, "/root/a /root/b /data/c /data/d");
    }

    #[test]
    fn leaves_tokens_literal_when_both_none() {
        let out = substitute_plugin_tokens(ALL_TOKENS, None, None);
        assert_eq!(out, ALL_TOKENS);
    }

    #[test]
    fn expands_only_root_when_data_none() {
        let out = substitute_plugin_tokens(ALL_TOKENS, Some("/root"), None);
        assert_eq!(
            out,
            "/root/a /root/b ${CLAUDE_PLUGIN_DATA}/c ${DS_PLUGIN_DATA}/d"
        );
    }

    #[test]
    fn agent_marker_constants_match_cursor_parity() {
        assert_eq!(DS_AGENT_ENV, "DS_AGENT");
        assert_eq!(DS_AGENT_ENV_VALUE, "1");
    }

    #[test]
    fn strip_model_credentials_removes_deepseek_and_keeps_harmless() {
        let mut env = HashMap::from([
            ("DEEPSEEK_API_KEY".into(), "sk-secret".into()),
            ("PATH".into(), "/usr/bin".into()),
            ("OPENAI_API_KEY".into(), "sk-openai".into()),
            ("HOME".into(), "/home/user".into()),
            ("deepseek_api_key".into(), "sk-lower".into()),
        ]);
        strip_model_credentials_from_map(&mut env);
        assert!(!env.contains_key("DEEPSEEK_API_KEY"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("deepseek_api_key"));
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/home/user"));
        assert!(MODEL_CREDENTIAL_ENV_KEYS.contains(&"DEEPSEEK_API_KEY"));
    }
}
