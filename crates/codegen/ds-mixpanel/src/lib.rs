//! Lightweight Mixpanel HTTP tracking client — **disabled in DS builds**.
//!
//! All network calls are no-ops so product analytics never leave the machine.
//! The types remain so call sites compile unchanged.

use std::collections::HashMap;

/// Mixpanel client for sending track events (network disabled).
#[derive(Clone)]
pub struct Mixpanel {
    token: String,
    client: reqwest::Client,
}

/// Error type for Mixpanel operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl Mixpanel {
    /// Create a new Mixpanel client with the given project token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Create a new Mixpanel client with a shared reqwest client.
    pub fn with_client(token: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            token: token.into(),
            client,
        }
    }

    fn prepare_properties(
        &self,
        mut properties: HashMap<String, serde_json::Value>,
    ) -> HashMap<String, serde_json::Value> {
        for v in properties.values_mut() {
            ds_secrets::redact_json_string_values(v);
        }
        properties.insert("token".to_owned(), serde_json::json!(self.token));
        properties
    }

    /// Track an event — **no network I/O** in DS builds.
    pub async fn track(
        &self,
        _event: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), Error> {
        // Scrub only (keeps redaction tests meaningful); never POST.
        let _ = self.prepare_properties(properties.unwrap_or_default());
        let _ = &self.client;
        Ok(())
    }

    /// Engage profile update — **no network I/O** in DS builds.
    pub async fn engage(
        &self,
        _distinct_id: &str,
        set: HashMap<String, serde_json::Value>,
    ) -> Result<(), Error> {
        let mut scrubbed = set;
        for v in scrubbed.values_mut() {
            ds_secrets::redact_json_string_values(v);
        }
        let _ = scrubbed;
        let _ = &self.client;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_properties_injects_token_after_scrub() {
        let mp = Mixpanel::new("test-token");
        let mut props = HashMap::new();
        props.insert("foo".into(), serde_json::json!("bar"));
        let out = mp.prepare_properties(props);
        assert_eq!(out.get("token").and_then(|v| v.as_str()), Some("test-token"));
    }
}
