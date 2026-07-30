//! Provider-qualified ChatGPT login/logout extension methods.

use agent_client_protocol as acp;

use super::{ExtResult, to_raw_response};
use crate::agent::MvpAgent;

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "ds.cli/chatgpt/login" => {
            let outcome = crate::chatgpt::login_and_load_models()
                .await
                .map_err(|error| {
                    acp::Error::internal_error().data(format!("ChatGPT sign-in failed: {error:#}"))
                })?;
            let model_count = outcome.models.len();
            agent.models_manager.install_chatgpt_models(outcome.models);
            let model_state = agent.models_manager.session_model_state();
            to_raw_response(&serde_json::json!({
                "ok": true,
                "modelCount": model_count,
                "modelState": model_state,
                "message": format!(
                    "ChatGPT sign-in complete: {}. Open /model to select a ChatGPT model.",
                    outcome.summary
                ),
            }))
        }
        "ds.cli/chatgpt/logout" => {
            let was_logged_in = crate::chatgpt::logout().map_err(|error| {
                acp::Error::internal_error().data(format!("ChatGPT sign-out failed: {error:#}"))
            })?;
            agent.models_manager.remove_chatgpt_models();
            let model_state = agent.models_manager.session_model_state();
            to_raw_response(&serde_json::json!({
                "ok": true,
                "wasLoggedIn": was_logged_in,
                "modelState": model_state,
                "message": if was_logged_in {
                    "Signed out of ChatGPT. DeepSeek credentials were not changed."
                } else {
                    "Not signed into ChatGPT. DeepSeek credentials were not changed."
                },
            }))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}
