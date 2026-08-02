//! Live ChatGPT Codex model catalog and account-scoped disk cache.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::config::{self, ModelEntry, ModelInfo};
use ds_sampler::AuthScheme;
use ds_sampling_types::{ApiBackend, ReasoningEffort, ReasoningEffortOption};

use super::auth::{self, Tokens};

pub(crate) const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
const CACHE_FILE: &str = "openai-codex-models.json";
const CACHE_VERSION: u8 = 1;
const MAX_CACHE_BYTES: u64 = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_MODELS: usize = 10_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ModelSpec {
    slug: String,
    display_name: String,
    description: String,
    context_window: u64,
    default_reasoning: Option<String>,
    reasoning: Vec<ReasoningSpec>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReasoningSpec {
    effort: String,
    description: String,
}

#[derive(Deserialize)]
struct RawCatalog {
    #[serde(default)]
    models: Vec<RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    slug: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    context_window: Option<u64>,
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<RawReasoning>,
}

#[derive(Deserialize)]
struct RawReasoning {
    effort: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct DiskCache {
    version: u8,
    account_id: String,
    models: Vec<ModelSpec>,
}

pub(crate) fn is_chatgpt_base_url(base_url: &str) -> bool {
    base_url.trim_end_matches('/') == CHATGPT_BASE_URL
}

pub(crate) fn cached_model_entries() -> anyhow::Result<IndexMap<String, ModelEntry>> {
    let Some(tokens) = auth::load_tokens()? else {
        return Ok(IndexMap::new());
    };
    let Some(cache) = read_cache()? else {
        return Ok(IndexMap::new());
    };
    if cache.account_id != tokens.account_id() {
        return Ok(IndexMap::new());
    }
    specs_to_entries(&cache.models, &tokens)
}

pub(crate) async fn refresh_model_entries() -> anyhow::Result<IndexMap<String, ModelEntry>> {
    let (tokens, specs) = fetch_specs().await?;
    write_cache(tokens.account_id(), &specs)?;
    specs_to_entries(&specs, &tokens)
}

pub(crate) fn describe_entries(entries: &IndexMap<String, ModelEntry>) -> Option<String> {
    let (id, entry) = entries.first()?;
    let effort = entry
        .info
        .reasoning_efforts
        .iter()
        .find(|option| option.default)
        .map(|option| option.id.clone())
        .unwrap_or_else(|| "provider default".to_owned());
    Some(format!(
        "{} models; default {id} ({} tokens, {effort} reasoning)",
        entries.len(),
        entry.info.context_window
    ))
}

async fn fetch_specs() -> anyhow::Result<(Tokens, Vec<ModelSpec>)> {
    let mut tokens = auth::ensure_fresh_tokens(false).await?;
    let mut response = fetch_with_tokens(&tokens).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        tokens = auth::ensure_fresh_tokens(true).await?;
        response = fetch_with_tokens(&tokens).await?;
    }
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        match auth::clear_tokens_if_current(&tokens) {
            Ok(true) => bail!(
                "ChatGPT rejected the saved session; run `ds login --chatgpt` to sign in again"
            ),
            Ok(false) => bail!(
                "the active ChatGPT session changed while its model catalog was loading; retry"
            ),
            Err(error) => {
                return Err(error).context(
                    "ChatGPT rejected the saved session, but its credentials could not be removed",
                );
            }
        }
    }
    let status = response.status();
    if !status.is_success() {
        bail!("ChatGPT model catalog returned HTTP {status}");
    }
    let raw: RawCatalog = auth::read_json_bounded(response, MAX_CATALOG_BYTES).await?;
    if raw.models.len() > MAX_MODELS {
        bail!("ChatGPT model catalog exceeded {MAX_MODELS} entries");
    }
    let specs = parse_catalog(raw.models);
    if specs.is_empty() {
        bail!("the ChatGPT account returned no usable Codex models");
    }
    Ok((tokens, specs))
}

async fn fetch_with_tokens(tokens: &Tokens) -> anyhow::Result<reqwest::Response> {
    let version = std::env::var("DS_OPENAI_CODEX_CLIENT_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "1.0.0".to_owned());
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("ds/{}", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(MODELS_URL)
        .query(&[("client_version", version)])
        .bearer_auth(tokens.access())
        .header("chatgpt-account-id", tokens.account_id())
        .header("originator", "codex_cli_rs")
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .context("fetch ChatGPT model catalog")
}

fn parse_catalog(raw: Vec<RawModel>) -> Vec<ModelSpec> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter_map(parse_model)
        .filter(|model| seen.insert(model.slug.clone()))
        .collect()
}

fn parse_model(raw: RawModel) -> Option<ModelSpec> {
    if raw.visibility.as_deref() != Some("list") {
        return None;
    }
    let slug = clean_text(raw.slug?, 128)?;
    let context_window = raw
        .context_window
        .filter(|window| (1..=10_000_000).contains(window))?;
    let display_name = raw
        .display_name
        .and_then(|value| clean_text(value, 256))
        .unwrap_or_else(|| slug.clone());
    let description = raw
        .description
        .and_then(|value| clean_text(value, 1024))
        .unwrap_or_default();

    let mut seen_efforts = std::collections::HashSet::new();
    let reasoning = raw
        .supported_reasoning_levels
        .into_iter()
        .filter_map(|item| {
            let effort = item.effort?;
            parse_wire_effort(&effort)?;
            if !seen_efforts.insert(effort.clone()) {
                return None;
            }
            Some(ReasoningSpec {
                effort,
                description: item
                    .description
                    .and_then(|value| clean_text(value, 512))
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let default_reasoning = match raw.default_reasoning_level {
        Some(effort) if reasoning.iter().any(|option| option.effort == effort) => Some(effort),
        // A provider default that ds-build cannot represent must stay omitted
        // on the wire. The backend will then apply that exact live default;
        // choosing a nearby local effort would silently downgrade it.
        Some(_) => None,
        None => reasoning
            .iter()
            .find(|option| option.effort == "medium")
            .or_else(|| reasoning.first())
            .map(|option| option.effort.clone()),
    };

    Some(ModelSpec {
        slug,
        display_name,
        description,
        context_window,
        default_reasoning,
        reasoning,
    })
}

fn clean_text(value: String, max_chars: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.chars().take(max_chars).collect())
}

fn parse_wire_effort(value: &str) -> Option<ReasoningEffort> {
    // DeepSeek product enum: none|low|high|max (parse accepts a few legacy aliases).
    value.parse().ok()
}

fn specs_to_entries(
    specs: &[ModelSpec],
    tokens: &Tokens,
) -> anyhow::Result<IndexMap<String, ModelEntry>> {
    let session_id = Uuid::new_v4().to_string();
    let mut entries = IndexMap::new();
    for spec in specs {
        let context_window = NonZeroU64::new(spec.context_window)
            .ok_or_else(|| anyhow!("ChatGPT model {} has no context window", spec.slug))?;
        let key = format!("openai/{}", spec.slug);
        let reasoning_efforts = spec
            .reasoning
            .iter()
            .filter_map(|option| {
                let value = parse_wire_effort(&option.effort)?;
                Some(ReasoningEffortOption {
                    id: option.effort.clone(),
                    value,
                    label: humanize_effort(&option.effort),
                    description: (!option.description.is_empty())
                        .then(|| option.description.clone()),
                    default: spec.default_reasoning.as_deref() == Some(&option.effort),
                })
            })
            .collect::<Vec<_>>();
        let reasoning_effort = spec
            .default_reasoning
            .as_deref()
            .and_then(parse_wire_effort);
        let mut extra_headers = IndexMap::new();
        extra_headers.insert(
            "chatgpt-account-id".to_owned(),
            tokens.account_id().to_owned(),
        );
        extra_headers.insert("originator".to_owned(), "codex_cli_rs".to_owned());
        extra_headers.insert(
            "OpenAI-Beta".to_owned(),
            "responses=experimental".to_owned(),
        );
        extra_headers.insert("session_id".to_owned(), session_id.clone());
        extra_headers.insert("conversation_id".to_owned(), session_id.clone());

        entries.insert(
            key.clone(),
            ModelEntry {
                info: ModelInfo {
                    id: Some(key),
                    model: spec.slug.clone(),
                    base_url: CHATGPT_BASE_URL.to_owned(),
                    name: Some(format!("ChatGPT · {}", spec.display_name)),
                    description: Some(if spec.description.is_empty() {
                        "ChatGPT subscription · Codex".to_owned()
                    } else {
                        format!("ChatGPT subscription · {}", spec.description)
                    }),
                    max_completion_tokens: None,
                    temperature: None,
                    top_p: None,
                    api_backend: ApiBackend::Responses,
                    auth_scheme: AuthScheme::Bearer,
                    extra_headers,
                    context_window,
                    auto_compact_threshold_percent: None,
                    system_prompt_label: None,
                    use_concise: false,
                    agent_type: config::default_agent_type(),
                    inference_idle_timeout_secs: None,
                    max_retries: None,
                    hidden: false,
                    user_selectable: true,
                    supported_in_api: true,
                    reasoning_effort,
                    supports_reasoning_effort: !reasoning_efforts.is_empty(),
                    reasoning_efforts,
                    supports_backend_search: false,
                    compactions_remaining: None,
                    compaction_at_tokens: None,
                    show_model_fingerprint: false,
                    stream_tool_calls: None,
                    laziness_detector: Default::default(),
                },
                // This construction-time value lets existing credential
                // resolution classify the model as provider-owned rather than
                // falling through to DeepSeek auth. Every actual request also
                // gets the live file-backed bearer resolver.
                api_key: Some(tokens.access().to_owned()),
                env_key: None,
                api_base_url: None,
            },
        );
    }
    Ok(entries)
}

fn humanize_effort(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn cache_path() -> PathBuf {
    crate::util::ds_home::ds_home().join(CACHE_FILE)
}

fn read_cache() -> anyhow::Result<Option<DiskCache>> {
    let path = cache_path();
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "ChatGPT model cache is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_CACHE_BYTES {
        bail!("ChatGPT model cache exceeded the size limit");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&path)?
        .take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        bail!("ChatGPT model cache exceeded the size limit");
    }
    let cache: DiskCache =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if cache.version != CACHE_VERSION
        || cache.account_id.is_empty()
        || cache.models.is_empty()
        || cache.models.len() > MAX_MODELS
        || cache.models.iter().any(|model| {
            model.slug.is_empty() || model.context_window == 0 || model.context_window > 10_000_000
        })
    {
        return Ok(None);
    }
    Ok(Some(cache))
}

fn write_cache(account_id: &str, models: &[ModelSpec]) -> anyhow::Result<()> {
    let path = cache_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ChatGPT model cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(&DiskCache {
        version: CACHE_VERSION,
        account_id: account_id.to_owned(),
        models: models.to_vec(),
    })?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        bail!("ChatGPT model cache exceeded the size limit");
    }
    let temp = parent.join(format!(
        ".{CACHE_FILE}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(windows)]
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error).with_context(|| format!("replace {}", path.display()));
        }
        std::fs::rename(&temp, &path)?;
        sync_directory(parent)?;
        Ok(())
    })();
    if let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %temp.display(), "could not remove model-cache temp file");
    }
    result
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_model() -> RawModel {
        RawModel {
            slug: Some("gpt-test".into()),
            display_name: Some("GPT Test".into()),
            description: Some("Research model".into()),
            visibility: Some("list".into()),
            context_window: Some(272_000),
            default_reasoning_level: Some("high".into()),
            supported_reasoning_levels: vec![
                RawReasoning {
                    effort: Some("low".into()),
                    description: Some("Fast".into()),
                },
                RawReasoning {
                    effort: Some("high".into()),
                    description: Some("Deep".into()),
                },
                RawReasoning {
                    effort: Some("ultra".into()),
                    description: Some("Composite".into()),
                },
            ],
        }
    }

    #[test]
    fn catalog_uses_live_context_and_supported_wire_efforts() {
        let spec = parse_model(raw_model()).unwrap();
        assert_eq!(spec.context_window, 272_000);
        assert_eq!(spec.default_reasoning.as_deref(), Some("high"));
        assert_eq!(
            spec.reasoning
                .iter()
                .map(|option| option.effort.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "high"]
        );
    }

    #[test]
    fn unrepresentable_live_default_is_left_to_the_provider() {
        let mut raw = raw_model();
        raw.default_reasoning_level = Some("max".into());
        let spec = parse_model(raw).unwrap();
        assert_eq!(spec.default_reasoning, None);

        let tokens = Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: u64::MAX,
            account_id: "acct".into(),
        };
        let entries = specs_to_entries(&[spec], &tokens).unwrap();
        let entry = entries.get("openai/gpt-test").unwrap();
        assert_eq!(entry.info.reasoning_effort, None);
        assert!(
            entry
                .info
                .reasoning_efforts
                .iter()
                .all(|option| !option.default)
        );
    }

    #[test]
    fn hidden_or_contextless_models_are_not_invented() {
        let mut hidden = raw_model();
        hidden.visibility = Some("hide".into());
        assert!(parse_model(hidden).is_none());

        let mut contextless = raw_model();
        contextless.context_window = None;
        assert!(parse_model(contextless).is_none());
    }

    #[test]
    fn entries_are_namespaced_and_do_not_route_through_deepseek() {
        let spec = parse_model(raw_model()).unwrap();
        let tokens = Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: u64::MAX,
            account_id: "acct".into(),
        };
        let entries = specs_to_entries(&[spec], &tokens).unwrap();
        let entry = entries.get("openai/gpt-test").unwrap();
        assert_eq!(entry.info.model, "gpt-test");
        assert_eq!(entry.info.base_url, CHATGPT_BASE_URL);
        assert_eq!(entry.info.api_backend, ApiBackend::Responses);
        assert_eq!(entry.info.context_window.get(), 272_000);
        assert_eq!(entry.info.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(
            entry.info.extra_headers.get("chatgpt-account-id"),
            Some(&"acct".to_owned())
        );
    }

    #[test]
    fn duplicate_slugs_keep_catalog_priority() {
        let first = raw_model();
        let mut second = raw_model();
        second.context_window = Some(128_000);
        let specs = parse_catalog(vec![first, second]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].context_window, 272_000);
    }
}
