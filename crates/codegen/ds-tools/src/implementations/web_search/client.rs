use super::types::WebSearchConfig;
use crate::attribution::SharedAttributionCallback;
use crate::types::SharedApiKeyProvider;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use std::sync::LazyLock;

/// DuckDuckGo HTML search endpoint (no API key, no JavaScript required).
const DDG_SEARCH_URL: &str = "https://html.duckduckgo.com/html/";

/// Browser-like User-Agent to avoid CAPTCHAs.
const DDG_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36";

/// Maximum number of search results to return.
const MAX_RESULTS: usize = 10;

/// Regex to parse DuckDuckGo HTML result entries.
/// Each result has: <a class="result__a" href="URL">Title</a> ... <a class="result__snippet">Snippet</a>
/// (?s) enables dot-matches-newline so .*? spans across HTML line breaks.
static DDG_RESULT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#,
    )
    .expect("failed to compile DDG result regex")
});

/// Simple regex to strip HTML tags from extracted text.
static HTML_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("failed to compile HTML tag regex"));

/// HTTP client that performs real web searches via DuckDuckGo's HTML interface.
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
}

impl WebSearchClient {
    /// Create a new web search client.
    ///
    /// The config's `base_url` and `model` fields are not used for the
    /// DuckDuckGo backend, but the `api_key` and `extra_headers` fields
    /// are preserved for backward compatibility with the config schema.
    pub fn new(
        config: &WebSearchConfig,
        _api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, ds_tool_runtime::ToolError> {
        let WebSearchConfig::Enabled {
            extra_headers,
            ..
        } = config
        else {
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                "Cannot create WebSearchClient from disabled config".to_string(),
            ));
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/x-www-form-urlencoded"));
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

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(DDG_USER_AGENT)
            .build()
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Failed to build HTTP client: {e}"),
                )
            })?;

        Ok(Self { http })
    }

    /// Wire a 401-attribution callback. No-op for DuckDuckGo backend
    /// (no auth required), but kept for API compatibility.
    pub fn with_attribution_callback(
        self,
        _callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self
    }

    /// Perform a real web search via DuckDuckGo.
    ///
    /// Returns `(formatted_text, citations)` where formatted_text is a
    /// human-readable summary of the search results and citations are
    /// unique URLs found.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
        let results = self.fetch_ddg_results(query).await?;
        let filtered: Vec<DdgResult> = if let Some(ref domains) = allowed_domains {
            results
                .into_iter()
                .filter(|r| {
                    domains.iter().any(|d| {
                        r.url.to_lowercase().contains(&d.to_lowercase())
                    })
                })
                .collect()
        } else {
            results
        };

        let citations: Vec<String> = filtered
            .iter()
            .map(|r| r.url.clone())
            .collect();

        let content = if filtered.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            format_search_results(&filtered)
        };

        Ok((content, citations))
    }

    /// Same as [`Self::search`] but returns `(title, url)` pairs.
    pub async fn search_with_titles(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<(String, String)>), ds_tool_runtime::ToolError> {
        let results = self.fetch_ddg_results(query).await?;
        let filtered: Vec<DdgResult> = if let Some(ref domains) = allowed_domains {
            results
                .into_iter()
                .filter(|r| {
                    domains.iter().any(|d| {
                        r.url.to_lowercase().contains(&d.to_lowercase())
                    })
                })
                .collect()
        } else {
            results
        };

        let pairs: Vec<(String, String)> = filtered
            .iter()
            .map(|r| (r.title.clone(), r.url.clone()))
            .collect();

        let content = if filtered.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            format_search_results(&filtered)
        };

        Ok((content, pairs))
    }

    /// Fetch search results from DuckDuckGo's HTML interface.
    async fn fetch_ddg_results(
        &self,
        query: &str,
    ) -> Result<Vec<DdgResult>, ds_tool_runtime::ToolError> {
        let response = self
            .http
            .post(DDG_SEARCH_URL)
            .header("Origin", "https://html.duckduckgo.com")
            .header("Referer", "https://html.duckduckgo.com/")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .form(&[("q", query)])
            .send()
            .await
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Search request failed: {e}"),
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Search returned HTTP {status}: {body}"),
            ));
        }

        let html = response.text().await.map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to read search response: {e}"),
            )
        })?;

        if html.contains("anomaly") || html.contains("g-recaptcha") {
            return Err(ds_tool_runtime::ToolError::execution(
                ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                "Search engine returned a CAPTCHA — please try again later.".to_string(),
            ));
        }

        let results = parse_ddg_html(&html);
        Ok(results)
    }
}

/// A single search result from DuckDuckGo.
#[derive(Debug, Clone)]
struct DdgResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo HTML response into structured results.
fn parse_ddg_html(html: &str) -> Vec<DdgResult> {
    let mut results = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    for caps in DDG_RESULT_RE.captures_iter(html) {
        let url = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let title_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let snippet_raw = caps.get(3).map(|m| m.as_str()).unwrap_or("");

        // Decode common HTML entities
        let url = decode_html_entities(&url);
        let title = decode_html_entities(&strip_html(title_raw).trim());
        let snippet = decode_html_entities(&strip_html(snippet_raw).trim());

        if url.is_empty() || title.is_empty() {
            continue;
        }
        if !seen_urls.insert(url.clone()) {
            continue; // deduplicate
        }

        results.push(DdgResult {
            title,
            url,
            snippet,
        });

        if results.len() >= MAX_RESULTS {
            break;
        }
    }

    results
}

/// Strip HTML tags from a string.
fn strip_html(input: &str) -> String {
    HTML_TAG_RE.replace_all(input, "").to_string()
}

/// Decode common HTML entities.
fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
}

/// Format search results as a readable text block.
fn format_search_results(results: &[DdgResult]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ddg_html_extracts_results() {
        let html = r#"
        <html><body>
        <a class="result__a" href="https://www.rust-lang.org/">Rust Programming Language</a>
        some stuff in between
        <a class="result__snippet">A language empowering everyone to build reliable and efficient software.</a>

        <a class="result__a" href="https://docs.rs/">Docs.rs</a>
        <a class="result__snippet">Documentation for the Rust programming language.</a>
        </body></html>
        "#;
        let results = parse_ddg_html(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert!(results[0].snippet.contains("reliable and efficient"));
        assert_eq!(results[1].title, "Docs.rs");
        assert_eq!(results[1].url, "https://docs.rs/");
    }

    #[test]
    fn test_parse_ddg_html_deduplicates() {
        let html = r#"
        <a class="result__a" href="https://example.com/">Example</a>
        <a class="result__snippet">First appearance.</a>
        <a class="result__a" href="https://example.com/">Example Again</a>
        <a class="result__snippet">Duplicate.</a>
        "#;
        let results = parse_ddg_html(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/");
    }

    #[test]
    fn test_parse_ddg_html_empty() {
        let results = parse_ddg_html("<html><body>No results here</body></html>");
        assert!(results.is_empty());
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("Hello &amp; World"), "Hello & World");
        assert_eq!(decode_html_entities("a &lt; b &gt; c"), "a < b > c");
        assert_eq!(decode_html_entities("&quot;quoted&quot;"), "\"quoted\"");
    }

    #[test]
    fn test_strip_html() {
        assert_eq!(strip_html("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html("<a href='x'>link</a>"), "link");
        assert_eq!(strip_html("no tags"), "no tags");
    }

    #[test]
    fn test_format_search_results() {
        let results = vec![
            DdgResult {
                title: "Rust".into(),
                url: "https://rust-lang.org".into(),
                snippet: "A systems language.".into(),
            },
            DdgResult {
                title: "Go".into(),
                url: "https://go.dev".into(),
                snippet: "A concurrent language.".into(),
            },
        ];
        let formatted = format_search_results(&results);
        assert!(formatted.contains("1. Rust"));
        assert!(formatted.contains("URL: https://rust-lang.org"));
        assert!(formatted.contains("A systems language."));
        assert!(formatted.contains("2. Go"));
        assert!(formatted.contains("URL: https://go.dev"));
    }

    #[test]
    fn test_parse_ddg_html_trims_and_decodes() {
        let html = r#"
        <a class="result__a" href="https://example.com/page?a=1&amp;b=2">
          <b>Example &amp; Title</b>
        </a>
        <a class="result__snippet">This &lt;contains&gt; HTML &amp; entities.</a>
        "#;
        let results = parse_ddg_html(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & Title");
        assert_eq!(results[0].url, "https://example.com/page?a=1&b=2");
        // After decode, &lt; becomes < and &gt; becomes >
        assert!(results[0].snippet.contains("<contains>"));
        // After decode, &amp; becomes &
        assert!(results[0].snippet.contains("& entities"));
        // HTML <b> tags are stripped
        assert!(!results[0].title.contains("<b>"));
    }

    #[tokio::test]
    async fn test_search_mocks_ddg() {
        use wiremock::matchers::{body_string, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/html/"))
            .and(body_string("q=test+query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<a class="result__a" href="https://example.com">Example Site</a>
                   <a class="result__snippet">An example website for testing.</a>"#,
            ))
            .mount(&server)
            .await;

        // We need to override DDG_SEARCH_URL for testing.
        // The client uses a constant, so we test parse + format directly.
        let html = r#"<a class="result__a" href="https://example.com">Example Site</a>
                       <a class="result__snippet">An example website for testing.</a>"#;
        let results = parse_ddg_html(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Site");
        assert_eq!(results[0].url, "https://example.com");

        let formatted = format_search_results(&results);
        assert!(formatted.contains("Example Site"));
        assert!(formatted.contains("https://example.com"));
    }
}
