//! Web search client — primary: Brave Search API (reliable REST API),
//! fallback: DuckDuckGo Instant Answer API (no API key needed, limited data).
//!
//! To enable Brave Search (recommended):
//!   export DS_BRAVE_API_KEY="BSA-..."   # free at https://api.search.brave.com/
//!
//! Without a Brave key, falls back to DDG Instant Answer API which works
//! for factual queries but returns limited structured data.

use super::types::WebSearchConfig;
use crate::attribution::SharedAttributionCallback;
use crate::types::SharedApiKeyProvider;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};

/// Brave Search API endpoint.
const BRAVE_API_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// DuckDuckGo Instant Answer API (fallback).
const DDG_API_URL: &str = "https://api.duckduckgo.com/";

/// Maximum search results.
const MAX_RESULTS: usize = 10;

/// HTTP client that performs web searches via Brave Search API (primary)
/// or DuckDuckGo Instant Answer API (fallback).
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
    brave_api_key: Option<String>,
}

impl WebSearchClient {
    pub fn new(
        config: &WebSearchConfig,
        _api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, ds_tool_runtime::ToolError> {
        let WebSearchConfig::Enabled { extra_headers, .. } = config else {
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                "Cannot create WebSearchClient from disabled config".to_string(),
            ));
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
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

        let brave_api_key = std::env::var("DS_BRAVE_API_KEY").ok()
            .filter(|k| !k.is_empty());

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("ds-build/1.0")
            .build()
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Failed to build HTTP client: {e}"),
                )
            })?;

        Ok(Self { http, brave_api_key })
    }

    pub fn with_attribution_callback(
        self,
        _callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self
    }

    /// Perform a web search. Uses Brave API if DS_BRAVE_API_KEY is set,
    /// otherwise falls back to DuckDuckGo Instant Answer API.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
        let results = if let Some(ref key) = self.brave_api_key {
            self.search_brave(query, allowed_domains.as_deref(), key).await?
        } else {
            self.search_ddg_api(query).await?
        };

        let citations: Vec<String> = results.iter().map(|r| r.url.clone()).collect();
        let content = if results.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            format_results(&results)
        };
        Ok((content, citations))
    }

    pub async fn search_with_titles(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<(String, String)>), ds_tool_runtime::ToolError> {
        let results = if let Some(ref key) = self.brave_api_key {
            self.search_brave(query, allowed_domains.as_deref(), key).await?
        } else {
            self.search_ddg_api(query).await?
        };

        let pairs: Vec<(String, String)> = results
            .iter()
            .map(|r| (r.title.clone(), r.url.clone()))
            .collect();
        let content = if results.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            format_results(&results)
        };
        Ok((content, pairs))
    }

    /// Search via Brave Search API.
    async fn search_brave(
        &self,
        query: &str,
        allowed_domains: Option<&[String]>,
        api_key: &str,
    ) -> Result<Vec<SearchResult>, ds_tool_runtime::ToolError> {
        let mut url = format!("{}?q={}&count={}", BRAVE_API_URL, url_encode(query), MAX_RESULTS);
        if let Some(domains) = allowed_domains {
            for d in domains {
                url.push_str(&format!("&site={}", d.trim()));
            }
        }

        let response = self.http
            .get(&url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Brave Search request failed: {e}"),
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Brave Search returned HTTP {status}: {body}"),
            ));
        }

        let body: serde_json::Value = response.json().await.map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to parse Brave Search response: {e}"),
            )
        })?;

        let mut results = Vec::new();
        if let Some(web) = body.get("web").and_then(|v| v.get("results")).and_then(|v| v.as_array()) {
            for item in web.iter().take(MAX_RESULTS) {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let snippet = item.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if !url.is_empty() && !title.is_empty() {
                    results.push(SearchResult { title, url, snippet });
                }
            }
        }

        Ok(results)
    }

    /// Fallback: DuckDuckGo Instant Answer API.
    async fn search_ddg_api(
        &self,
        query: &str,
    ) -> Result<Vec<SearchResult>, ds_tool_runtime::ToolError> {
        let url = format!("{}?q={}&format=json&no_html=1&no_redirect=1", DDG_API_URL, url_encode(query));
        let response = self.http
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("DDG API request failed: {e}"),
                )
            })?;

        let body: serde_json::Value = response.json().await.map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to parse DDG API response: {e}"),
            )
        })?;

        Ok(parse_ddg_api(&body))
    }
}

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

fn url_encode(s: &str) -> String {
    s.replace(' ', "+")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
        .replace('?', "%3F")
}

fn format_results(results: &[SearchResult]) -> String {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, r.title));
        out.push_str(&format!("   URL: {}\n", r.url));
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn parse_ddg_api(body: &serde_json::Value) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let (Some(text), Some(url), Some(source)) = (
        body.get("AbstractText").and_then(|v| v.as_str()).filter(|s| !s.is_empty()),
        body.get("AbstractURL").and_then(|v| v.as_str()),
        body.get("AbstractSource").and_then(|v| v.as_str()),
    ) {
        let title = body.get("Heading").and_then(|v| v.as_str()).unwrap_or(source);
        if seen.insert(url.to_string()) {
            results.push(SearchResult {
                title: format!("{} ({})", title, source),
                url: url.to_string(),
                snippet: text.to_string(),
            });
        }
    }

    if let Some(topics) = body.get("RelatedTopics").and_then(|v| v.as_array()) {
        for topic in topics {
            if let (Some(text), Some(url)) = (
                topic.get("Text").and_then(|v| v.as_str()),
                topic.get("FirstURL").and_then(|v| v.as_str()),
            ) {
                if seen.insert(url.to_string()) {
                    results.push(SearchResult {
                        title: text.to_string(),
                        url: url.to_string(),
                        snippet: String::new(),
                    });
                }
            }
        }
    }

    results.truncate(MAX_RESULTS);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("test#frag?x=1"), "test%23frag%3Fx%3D1");
    }

    #[test]
    fn test_format_results() {
        let results = vec![
            SearchResult {
                title: "Rust".into(),
                url: "https://rust-lang.org".into(),
                snippet: "A systems language.".into(),
            },
        ];
        let formatted = format_results(&results);
        assert!(formatted.contains("1. Rust"));
        assert!(formatted.contains("URL: https://rust-lang.org"));
        assert!(formatted.contains("A systems language."));
    }

    #[test]
    fn test_parse_ddg_api_empty() {
        let body = serde_json::json!({});
        let results = parse_ddg_api(&body);
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_ddg_api_with_abstract() {
        let body = serde_json::json!({
            "Heading": "Rust",
            "AbstractSource": "Wikipedia",
            "AbstractURL": "https://en.wikipedia.org/wiki/Rust",
            "AbstractText": "Rust is a programming language."
        });
        let results = parse_ddg_api(&body);
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("Wikipedia"));
        assert_eq!(results[0].url, "https://en.wikipedia.org/wiki/Rust");
    }

    #[test]
    fn test_parse_ddg_api_with_topics() {
        let body = serde_json::json!({
            "RelatedTopics": [
                {"Text": "Topic One", "FirstURL": "https://example.com/1"},
                {"Text": "Topic Two", "FirstURL": "https://example.com/2"}
            ]
        });
        let results = parse_ddg_api(&body);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Topic One");
        assert_eq!(results[1].url, "https://example.com/2");
    }
}
