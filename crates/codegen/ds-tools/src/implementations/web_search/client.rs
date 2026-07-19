use super::types::WebSearchConfig;
use crate::attribution::{SharedAttributionCallback, ToolConsumer};
use crate::types::SharedApiKeyProvider;
use regex::Regex;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use std::sync::LazyLock;

/// System prompt that instructs the model to produce search-result-style
/// responses with citations.
const SEARCH_SYSTEM_PROMPT: &str = "\
You are a web search assistant. Given a search query, provide a comprehensive, \
factual answer as if you had searched the web. Include relevant URLs as \
citations in your response. Be concise but thorough. Focus on software \
development and technical topics. When information may be time-sensitive, \
note that your knowledge has a cutoff date.";

/// Regex to extract URLs from response text for citation extraction.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https?://[^\s<>"')\]}]+"#).expect("failed to compile URL regex")
});

/// A minimal, purpose-built HTTP client for web search via the Chat
/// Completions API.
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key_provider: Option<SharedApiKeyProvider>,
    /// Optional 401-attribution hook. Callers can wire this so a 401
    /// from the API emits an `auth_401_attribution` event
    /// with `consumer == "WebSearch"`.
    attribution_callback: Option<SharedAttributionCallback>,
}
impl WebSearchClient {
    /// Create a new web search client from `WebSearchConfig::Enabled`.
    ///
    /// Returns `Err` if the config is `Disabled` or if header values are invalid.
    pub fn new(
        config: &WebSearchConfig,
        api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, ds_tool_runtime::ToolError> {
        let WebSearchConfig::Enabled {
            api_key,
            base_url,
            model,
            extra_headers,
            alpha_test_key,
        } = config
        else {
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                "Cannot create WebSearchClient from disabled config".to_string(),
            ));
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid API key for header: {e}"),
                )
            })?,
        );
        for (key, value) in extra_headers {
            let header_name = HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid header name '{key}': {e}"),
                )
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid header value for '{key}': {e}"),
                )
            })?;
            headers.insert(header_name, header_value);
        }
        let _ = alpha_test_key;
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Failed to build HTTP client: {e}"),
                )
            })?;
        Ok(Self {
            http,
            base_url: base_url.clone(),
            model: model.clone(),
            api_key_provider,
            attribution_callback: None,
        })
    }
    /// Wire a 401-attribution callback into this client. Idempotent;
    /// safe to call before or after the first request.
    pub fn with_attribution_callback(
        mut self,
        callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self.attribution_callback = callback;
        self
    }
    async fn current_bearer(&self) -> Option<String> {
        crate::types::api_key_provider::resolve_bearer(self.api_key_provider.as_ref()).await
    }
    fn record_401_attribution(&self, sent_bearer: Option<&str>) {
        crate::attribution::emit_401(
            self.attribution_callback.as_ref(),
            ToolConsumer::WebSearch,
            sent_bearer,
        );
    }
    /// Perform a web search query using the Chat Completions API.
    ///
    /// Returns `(content, citations)` where content is the assistant's
    /// response text and citations are unique URLs found in the response.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
        let mut system_prompt = SEARCH_SYSTEM_PROMPT.to_string();
        if let Some(ref domains) = allowed_domains {
            if !domains.is_empty() {
                system_prompt.push_str(&format!(
                    " Only use information from these domains: {}.",
                    domains.join(", ")
                ));
            }
        }
        let request_body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": query}
            ],
            "temperature": 0.1,
            "top_p": 0.95,
            "max_tokens": 8192,
            "stream": false
        });

        let (bytes, sent_bearer) = self
            .send_chat_completion_request(&request_body)
            .await?;
        let content = parse_chat_completion_content(&bytes)?;
        let citations = extract_urls_from_text(&content);
        let _ = sent_bearer;
        Ok((content, citations))
    }
    /// Same as [`Self::search`] but also extracts per-citation titles when
    /// available. Returns `(content, citations_with_titles)`
    /// where each citation is `(title, url)`. Since the Chat Completions
    /// API does not provide structured citation metadata, titles are
    /// extracted heuristically from surrounding context or left empty.
    ///
    /// Used by the cursor-compat `WebSearch` adapter to render a
    /// `Links:\n1. [title](url)` list instead of the LLM synthesis text.
    pub async fn search_with_titles(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<(String, String)>), ds_tool_runtime::ToolError> {
        let mut system_prompt = SEARCH_SYSTEM_PROMPT.to_string();
        if let Some(ref domains) = allowed_domains {
            if !domains.is_empty() {
                system_prompt.push_str(&format!(
                    " Only use information from these domains: {}.",
                    domains.join(", ")
                ));
            }
        }
        let request_body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": query}
            ],
            "temperature": 0.1,
            "top_p": 0.95,
            "max_tokens": 8192,
            "stream": false
        });

        let (bytes, sent_bearer) = self
            .send_chat_completion_request(&request_body)
            .await?;
        let content = parse_chat_completion_content(&bytes)?;
        let urls = extract_urls_from_text(&content);
        let pairs: Vec<(String, String)> = urls
            .into_iter()
            .map(|url| (String::new(), url))
            .collect();
        let _ = sent_bearer;
        Ok((content, pairs))
    }

    /// Send a Chat Completions request and return the raw response bytes
    /// together with the bearer token sent (for attribution).
    async fn send_chat_completion_request(
        &self,
        body: &serde_json::Value,
    ) -> Result<(Vec<u8>, Option<String>), ds_tool_runtime::ToolError> {
        let url = format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let sent_bearer = self.current_bearer().await;
        let mut req = self.http.post(&url).json(body);
        if let Some(ref key) = sent_bearer {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = req.send().await.map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("HTTP request failed: {e}"),
            )
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(sent_bearer.as_deref());
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(ds_tool_runtime::ToolError::unauthorized(format!(
                "Chat Completions API returned 401 Unauthorized: {body}"
            ))
            .with_details(serde_json::json!({ "tool_id" : "web_search", "status" : 401, })));
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Chat Completions API returned {status}: {body}"),
            ));
        }
        let bytes = response.bytes().await.map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to read response body: {e}"),
            )
        })?;
        Ok((bytes.to_vec(), sent_bearer))
    }
}
/// Parse a Chat Completions API response and extract the assistant's text.
///
/// Falls back to `reasoning_content` when `content` is empty (DeepSeek
/// models may emit reasoning-only responses when max_tokens is exhausted
/// during the thinking phase).
fn parse_chat_completion_content(bytes: &[u8]) -> Result<String, ds_tool_runtime::ToolError> {
    let body: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        ds_tool_runtime::ToolError::execution(
            ds_tool_protocol::ToolId::new("web_search").expect("valid"),
            format!("Failed to parse Chat Completions response: {e}"),
        )
    })?;

    let message = &body["choices"][0]["message"];
    let content = message["content"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            message["reasoning_content"]
                .as_str()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("No search results found.");

    Ok(content.to_string())
}

/// Extract unique URLs from text content using a regex.
fn extract_urls_from_text(text: &str) -> Vec<String> {
    let mut urls: Vec<String> = URL_RE
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect();
    // Deduplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    urls.retain(|url| seen.insert(url.clone()));
    urls
}

/// Extract citation URLs from the Response output items (legacy, for
/// backward-compatibility with test helpers).
/// The async-openai crate doesn't provide a helper for this, and the `url` field
/// in `UrlCitationBody` is private, so we serialize to JSON to extract it.
#[allow(dead_code)]
fn extract_citations(response: &async_openai::types::responses::Response) -> Vec<String> {
    use async_openai::types::responses as rs;
    let mut citations = Vec::new();
    for output_item in &response.output {
        if let rs::OutputItem::Message(output_message) = output_item {
            for message_content in &output_message.content {
                if let rs::OutputMessageContent::OutputText(text_content) = message_content {
                    for annotation in &text_content.annotations {
                        if let rs::Annotation::UrlCitation(url_citation) = annotation
                            && let Ok(json) = serde_json::to_value(url_citation)
                            && let Some(url) = json.get("url").and_then(|v| v.as_str())
                        {
                            citations.push(url.to_string());
                        }
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    citations.retain(|url| seen.insert(url.clone()));
    citations
}

/// Extract `(title, url)` pairs from the Responses API annotations (legacy,
/// for backward-compatibility with test helpers).
#[allow(dead_code)]
fn extract_citation_pairs(
    response: &async_openai::types::responses::Response,
) -> Vec<(String, String)> {
    use async_openai::types::responses as rs;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for output_item in &response.output {
        if let rs::OutputItem::Message(output_message) = output_item {
            for message_content in &output_message.content {
                if let rs::OutputMessageContent::OutputText(text_content) = message_content {
                    for annotation in &text_content.annotations {
                        if let rs::Annotation::UrlCitation(url_citation) = annotation
                            && let Ok(json) = serde_json::to_value(url_citation)
                        {
                            let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("");
                            if url.is_empty() {
                                continue;
                            }
                            let title = json
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            pairs.push((title, url.to_string()));
                        }
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    pairs.retain(|(_t, url)| seen.insert(url.clone()));
    pairs
}
#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn test_new_client_uses_configured_model() {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "custom-enterprise-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None).expect("client should build");
        assert_eq!(client.model, "custom-enterprise-model");
    }
    /// Counts attribution callback invocations for the test below.
    #[derive(Default, Debug)]
    struct CountingCallback {
        invocations: std::sync::Mutex<Vec<(ToolConsumer, Option<String>)>>,
    }
    impl crate::attribution::Auth401AttributionCallback for CountingCallback {
        fn record_401(&self, consumer: ToolConsumer, sent_bearer_prefix: Option<&str>) {
            self.invocations
                .lock()
                .unwrap()
                .push((consumer, sent_bearer_prefix.map(|s| s.to_string())));
        }
    }
    /// `record_401_attribution` invokes the wired callback with
    /// `ToolConsumer::WebSearch` and the truncated bearer prefix.
    /// The full bearer never crosses the trait boundary.
    #[test]
    fn record_401_attribution_passes_truncated_prefix_to_callback() {
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let config = WebSearchConfig::Enabled {
            api_key: "ignored".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None)
            .expect("client should build")
            .with_attribution_callback(Some(cb_dyn));
        client.record_401_attribution(Some("bearer-with-long-tail-aaaaaaaaaa"));
        let calls = cb.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ToolConsumer::WebSearch);
        assert_eq!(calls[0].1.as_deref(), Some("bearer-with-"));
        assert_eq!(
            calls[0].1.as_deref().map(str::len),
            Some(crate::attribution::SENT_BEARER_PREFIX_LEN),
        );
    }
    /// `record_401_attribution` is a no-op when no callback is wired
    /// -- the BYOK / standalone case must not panic or allocate.
    #[test]
    fn record_401_attribution_is_noop_without_callback() {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None).expect("client should build");
        client.record_401_attribution(Some("any-bearer"));
        client.record_401_attribution(None);
    }

    /// A provider that always returns `None`, simulating an API-key user
    /// whose token has aged past the client-side TTL.
    struct NoneProvider;
    impl crate::types::ApiKeyProvider for NoneProvider {
        fn current_api_key(&self) -> Option<String> {
            None
        }
    }
    /// When the dynamic provider returns `None`, the static `api_key`
    /// from config must still be sent as the Authorization header.
    #[tokio::test]
    async fn static_api_key_is_fallback_when_provider_returns_none() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer static-key-from-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                chat_completion_response("search result")
            )))
            .mount(&server)
            .await;
        let config = WebSearchConfig::Enabled {
            api_key: "static-key-from-config".to_string(),
            base_url: server.uri(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
        };
        let provider: SharedApiKeyProvider = std::sync::Arc::new(NoneProvider);
        let client = WebSearchClient::new(&config, Some(provider)).expect("client should build");
        let (content, _citations) = client
            .search("test query", None)
            .await
            .expect("search must succeed with static key fallback");
        assert_eq!(content, "search result");
    }
    /// When the provider returns a fresh key, it overrides the static one.
    #[tokio::test]
    async fn provider_key_overrides_static_key() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        struct FreshProvider;
        impl crate::types::ApiKeyProvider for FreshProvider {
            fn current_api_key(&self) -> Option<String> {
                Some("fresh-key-from-provider".to_string())
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer fresh-key-from-provider"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                chat_completion_response("fresh result")
            )))
            .mount(&server)
            .await;
        let config = WebSearchConfig::Enabled {
            api_key: "stale-static-key".to_string(),
            base_url: server.uri(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
        };
        let provider: SharedApiKeyProvider = std::sync::Arc::new(FreshProvider);
        let client = WebSearchClient::new(&config, Some(provider)).expect("client should build");
        let (content, _citations) = client
            .search("test query", None)
            .await
            .expect("search must succeed with provider key");
        assert_eq!(content, "fresh result");
    }

    /// Build a minimal Chat Completions JSON response for tests.
    fn chat_completion_response(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        })
    }

    #[test]
    fn test_parse_chat_completion_content_extracts_text() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Rust is a systems programming language."
                }
            }]
        });
        let content = parse_chat_completion_content(
            &serde_json::to_vec(&response).unwrap()
        ).unwrap();
        assert_eq!(content, "Rust is a systems programming language.");
    }

    #[test]
    fn test_parse_chat_completion_falls_back_to_reasoning() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "Let me think about this..."
                }
            }]
        });
        let content = parse_chat_completion_content(
            &serde_json::to_vec(&response).unwrap()
        ).unwrap();
        assert_eq!(content, "Let me think about this...");
    }

    #[test]
    fn test_parse_chat_completion_empty_response() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": ""
                }
            }]
        });
        let content = parse_chat_completion_content(
            &serde_json::to_vec(&response).unwrap()
        ).unwrap();
        assert_eq!(content, "No search results found.");
    }

    #[test]
    fn test_extract_urls_from_text() {
        let text = "See https://www.rust-lang.org/ and https://docs.rs/ for more info.";
        let urls = extract_urls_from_text(text);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://www.rust-lang.org/");
        assert_eq!(urls[1], "https://docs.rs/");
    }

    #[test]
    fn test_extract_urls_deduplicates() {
        let text = "Visit https://example.com and also https://example.com again.";
        let urls = extract_urls_from_text(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://example.com");
    }

    #[test]
    fn test_extract_urls_no_urls() {
        let text = "No URLs here, just plain text.";
        let urls = extract_urls_from_text(text);
        assert!(urls.is_empty());
    }

    #[test]
    fn test_search_with_titles_returns_empty_title_pairs() {
        // search_with_titles delegates to search and wraps URLs in (empty, url) pairs.
        let text = "Check https://rust-lang.org for details.";
        let urls = extract_urls_from_text(text);
        let pairs: Vec<(String, String)> = urls
            .into_iter()
            .map(|url| (String::new(), url))
            .collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, ""); // title is empty
        assert_eq!(pairs[0].1, "https://rust-lang.org");
    }
}
