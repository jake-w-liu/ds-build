//! Optional ChatGPT-subscription provider.
//!
//! Everything here is additive. DeepSeek auth, endpoints, model defaults, and
//! command behavior remain owned by their existing modules.

mod auth;
mod catalog;

use anyhow::Context as _;
use indexmap::IndexMap;

use crate::agent::config::ModelEntry;

pub(crate) use catalog::is_chatgpt_base_url;

pub(crate) struct LoginOutcome {
    pub(crate) models: IndexMap<String, ModelEntry>,
    pub(crate) summary: String,
}

pub(crate) fn cached_model_entries() -> anyhow::Result<IndexMap<String, ModelEntry>> {
    catalog::cached_model_entries()
}

pub(crate) async fn login_and_load_models() -> anyhow::Result<LoginOutcome> {
    let tokens = auth::login().await?;
    let models = match catalog::refresh_model_entries().await {
        Ok(models) => models,
        Err(error) => {
            if let Err(clear_error) = auth::clear_tokens_if_current(&tokens) {
                return Err(error.context(format!(
                    "also failed to remove the incomplete ChatGPT login: {clear_error}"
                )));
            }
            return Err(error);
        }
    };
    auth::spawn_refresh_loop();
    let summary =
        catalog::describe_entries(&models).unwrap_or_else(|| "ChatGPT signed in".to_owned());
    Ok(LoginOutcome { models, summary })
}

pub(crate) fn logout() -> anyhow::Result<bool> {
    auth::clear_tokens()
}

pub(crate) fn attach_bearer_resolver(config: &mut ds_sampler::SamplerConfig) {
    if is_chatgpt_base_url(&config.base_url) {
        // DeepSeek account/deployment identifiers are meaningful only to the
        // DeepSeek transport and must never be forwarded to another provider.
        config.user_id = None;
        config.deployment_id = None;
        let expected_account_id = config
            .extra_headers
            .get("chatgpt-account-id")
            .cloned()
            .unwrap_or_default();
        config.bearer_resolver = Some(std::sync::Arc::new(auth::ChatgptBearerResolver::new(
            expected_account_id,
        )));
    }
}

pub(crate) fn start_background_sync(models_manager: crate::agent::models::ModelsManager) {
    if auth::load_tokens().ok().flatten().is_none() {
        return;
    }
    auth::spawn_refresh_loop();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        match catalog::refresh_model_entries().await {
            Ok(models) => models_manager.install_chatgpt_models(models),
            Err(error) => {
                tracing::warn!(%error, "could not refresh ChatGPT model catalog");
            }
        }
    });
}

pub async fn run_cli_login() -> anyhow::Result<()> {
    let outcome = login_and_load_models().await?;
    println!("ChatGPT sign-in complete: {}.", outcome.summary);
    println!("Use `ds --model openai/<model>` or `/model` to select it.");
    Ok(())
}

pub fn run_cli_logout() -> anyhow::Result<()> {
    let removed = logout()?;
    if removed {
        println!("Signed out of ChatGPT. DeepSeek credentials were not changed.");
    } else {
        println!("Not signed into ChatGPT. DeepSeek credentials were not changed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_attachment_strips_deepseek_identity_and_wires_live_bearer() {
        let mut config = ds_sampler::SamplerConfig {
            base_url: catalog::CHATGPT_BASE_URL.to_owned(),
            user_id: Some("deepseek-user".into()),
            deployment_id: Some("deepseek-deployment".into()),
            ..Default::default()
        };
        config
            .extra_headers
            .insert("chatgpt-account-id".into(), "acct".into());
        attach_bearer_resolver(&mut config);
        assert!(config.user_id.is_none());
        assert!(config.deployment_id.is_none());
        assert!(config.bearer_resolver.is_some());
    }

    #[test]
    fn deepseek_configuration_is_a_noop() {
        let mut config = ds_sampler::SamplerConfig {
            base_url: "https://api.deepseek.com/v1".into(),
            user_id: Some("deepseek-user".into()),
            deployment_id: Some("deepseek-deployment".into()),
            ..Default::default()
        };
        attach_bearer_resolver(&mut config);
        assert_eq!(config.user_id.as_deref(), Some("deepseek-user"));
        assert_eq!(config.deployment_id.as_deref(), Some("deepseek-deployment"));
        assert!(config.bearer_resolver.is_none());
    }
}
