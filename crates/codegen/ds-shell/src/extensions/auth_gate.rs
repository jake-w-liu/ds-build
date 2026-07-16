use agent_client_protocol as acp;

use crate::auth::{AuthManager, DsAuth};

/// Require DeepSeek auth from a sync context, accepting tokens in the client-side buffer window.
pub(crate) fn require_ds_auth(
    auth_manager: &AuthManager,
    missing_message: &'static str,
    non_ds_message: &'static str,
) -> Result<DsAuth, acp::Error> {
    let auth = auth_manager
        .current_or_expired()
        .ok_or_else(|| acp::Error::auth_required().data(missing_message))?;
    if !auth.is_ds_auth() {
        return Err(acp::Error::auth_required().data(non_ds_message));
    }
    Ok(auth)
}
