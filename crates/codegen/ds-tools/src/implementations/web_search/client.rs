//! Web search client — configured backend (Responses API) with a
//! DuckDuckGo HTML scraping fallback.
//!
//! When `WebSearchConfig::Enabled` carries a `base_url`/`api_key`/`model`,
//! `search()` first calls that backend's `/responses` endpoint with a
//! `web_search` hosted tool (the documented contract: "Calls the Responses
//! API with web search capability"). If the backend request fails (network,
//! auth, unsupported endpoint), the client falls back to DuckDuckGo HTML
//! scraping via POST.
//!
//! DDG requires no API key and uses `native-tls` (OS TLS stack) so the TLS
//! fingerprint matches a real browser. The DDG HTML endpoint blocks GET
//! requests with a visual CAPTCHA ("select all squares containing a duck"),
//! but POST requests with browser-like headers and a Referer consistently
//! return real results.
//!
//! The DDG Instant Answer API (`api.duckduckgo.com`) is intentionally NOT
//! used: it only returns structured data for dictionary/definition queries,
//! not general web search.
//!
//! Rate limit: the HTML endpoint is tolerant of moderate usage but may
//! challenge excessive request rates. Keep queries spaced reasonably.

use super::types::WebSearchConfig;
use crate::attribution::SharedAttributionCallback;
use crate::types::SharedApiKeyProvider;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

/// DuckDuckGo HTML search endpoint (non-JS version, POST required).
const DDG_HTML_URL: &str = "https://html.duckduckgo.com/html/";

/// Maximum search results.
const MAX_RESULTS: usize = 10;

/// Delay before DDG requests to avoid rate limiting (milliseconds).
const DDG_REQUEST_DELAY_MS: u64 = 200;

/// Max retries on rate-limit / bot-challenge responses.
const DDG_MAX_RETRIES: usize = 2;

/// Backoff multiplier (seconds) between retries.
const DDG_RETRY_BACKOFF_SECS: u64 = 2;

/// HTTP client that performs web searches: configured Responses backend
/// first, DuckDuckGo HTML scraping as the fallback.
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
    backend: Option<WebSearchBackend>,
}

/// Configured Responses-API backend (the `Enabled` config's endpoint).
#[derive(Debug, Clone)]
struct WebSearchBackend {
    base_url: String,
    api_key: String,
    model: String,
}

impl WebSearchClient {
    pub fn new(
        config: &WebSearchConfig,
        _api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, ds_tool_runtime::ToolError> {
        let WebSearchConfig::Enabled {
            api_key,
            base_url,
            model,
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
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (key, value) in extra_headers {
            let header_name =
                reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
                    ds_tool_runtime::ToolError::execution(
                        ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                        format!("Invalid header name '{key}': {e}"),
                    )
                })?;
            let header_value = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid header value for '{key}': {e}"),
                )
            })?;
            headers.insert(header_name, header_value);
        }

        // Browser-like User-Agent to avoid bot detection.
        // native-tls (OS SecureTransport / OpenSSL / SChannel) provides a
        // TLS fingerprint that matches real browsers, unlike rustls.
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .build()
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    ds_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Failed to build HTTP client: {e}"),
                )
            })?;

        let backend = if base_url.trim().is_empty() {
            None
        } else {
            Some(WebSearchBackend {
                base_url: base_url.trim_end_matches('/').to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
            })
        };

        Ok(Self { http, backend })
    }

    pub fn with_attribution_callback(
        self,
        _callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self
    }

    /// Perform a web search: configured backend first, DDG on failure.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
        if let Some(backend) = &self.backend {
            match self
                .search_via_backend(backend, query, allowed_domains.as_deref())
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "web_search backend failed; falling back to DuckDuckGo"
                    );
                }
            }
        }

        let results = self
            .search_ddg_html(query, allowed_domains.as_deref())
            .await?;

        let citations: Vec<String> = results.iter().map(|r| r.url.clone()).collect();
        let content = if results.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            format_results(&results)
        };
        Ok((content, citations))
    }

    /// Search via the configured Responses-API backend with a `web_search`
    /// hosted tool. Returns the concatenated answer text plus any citation
    /// URLs from output annotations.
    async fn search_via_backend(
        &self,
        backend: &WebSearchBackend,
        query: &str,
        allowed_domains: Option<&[String]>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
        let tool_id = ds_tool_protocol::ToolId::new("web_search").expect("valid");
        let mut tools_json = serde_json::json!({
            "type": "web_search",
            "web_search": {},
        });
        if let Some(domains) = allowed_domains.filter(|d| !d.is_empty()) {
            tools_json["web_search"]["filters"] = serde_json::json!({
                "allowed_domains": domains,
            });
        }
        let body = serde_json::json!({
            "model": backend.model,
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": query,
                }],
            }],
            "tools": [tools_json],
            "stream": false,
        });

        let url = format!("{}/responses", backend.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&backend.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ds_tool_runtime::ToolError::execution(
                    tool_id.clone(),
                    format!("backend search request failed: {e}"),
                )
            })?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ds_tool_runtime::ToolError::execution(
                tool_id.clone(),
                format!("backend search returned HTTP {status}: {text}"),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            ds_tool_runtime::ToolError::execution(
                tool_id.clone(),
                format!("backend search returned unparseable response: {e}"),
            )
        })?;

        // Collect message output items: output_text parts become content,
        // url_citation annotations become citations (verify() requires at
        // least one citation for web_search).
        let mut content_parts: Vec<String> = Vec::new();
        let mut citations: Vec<String> = Vec::new();
        if let Some(output) = value.get("output").and_then(|v| v.as_array()) {
            for item in output {
                if item.get("type").and_then(|t| t.as_str()) != Some("message") {
                    continue;
                }
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    for part in content {
                        match part.get("type").and_then(|t| t.as_str()) {
                            Some("output_text") => {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    content_parts.push(text.to_string());
                                }
                            }
                            _ => {}
                        }
                        if let Some(anns) = part.get("annotations").and_then(|a| a.as_array()) {
                            for ann in anns {
                                if let Some(url) = ann.get("url").and_then(|u| u.as_str()) {
                                    citations.push(url.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        let content = if content_parts.is_empty() {
            format!("No search results found for query: {query}")
        } else {
            content_parts.join("\n")
        };
        Ok((content, citations))
    }

    pub async fn search_with_titles(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<(String, String)>), ds_tool_runtime::ToolError> {
        let results = self
            .search_ddg_html(query, allowed_domains.as_deref())
            .await?;

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

    /// Search via DuckDuckGo HTML endpoint using POST.
    ///
    /// The HTML endpoint returns a simple non-JS page with result links
    /// (`class="result__a"`) and snippets (`class="result__snippet"`).
    /// URLs are DDG redirect links; we extract and decode the `uddg` param
    /// to get the real destination URL.
    ///
    /// Uses POST with form data (`q=...&b=`) instead of GET because DDG
    /// now blocks GET with a visual CAPTCHA challenge. POST with a Referer
    /// header consistently returns real search results.
    ///
    /// Includes a small pre-request delay and retry-with-backoff on
    /// rate-limit / bot-challenge responses.
    async fn search_ddg_html(
        &self,
        query: &str,
        allowed_domains: Option<&[String]>,
    ) -> Result<Vec<SearchResult>, ds_tool_runtime::ToolError> {
        // Build query — prepend site: restrictions if any
        let q = if let Some(domains) = allowed_domains {
            let sites: Vec<String> = domains
                .iter()
                .map(|d| format!("site:{}", d.trim()))
                .collect();
            format!("{} {}", sites.join(" "), query)
        } else {
            query.to_string()
        };

        let tool_id = ds_tool_protocol::ToolId::new("web_search").expect("valid");

        for attempt in 0..=DDG_MAX_RETRIES {
            // Small delay before each attempt to avoid rate limiting
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(
                    DDG_RETRY_BACKOFF_SECS * attempt as u64,
                ))
                .await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(DDG_REQUEST_DELAY_MS)).await;
            }

            let response = self
                .http
                .post(DDG_HTML_URL)
                .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Referer", "https://html.duckduckgo.com/")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .form(&[("q", q.as_str()), ("b", "")])
                .send()
                .await
                .map_err(|e| {
                    ds_tool_runtime::ToolError::execution(
                        tool_id.clone(),
                        format!("DDG HTML request failed: {e}"),
                    )
                })?;

            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if !status.is_success() {
                if attempt < DDG_MAX_RETRIES {
                    continue;
                }
                return Err(ds_tool_runtime::ToolError::execution(
                    tool_id.clone(),
                    format!("DDG HTML returned HTTP {status}"),
                ));
            }

            // Check for bot-detection challenge — retry if possible
            if body.contains("botnet")
                || body.contains("captcha")
                || body.contains("Unfortunately, bots use DuckDuckGo")
            {
                if attempt < DDG_MAX_RETRIES {
                    continue;
                }
                return Err(ds_tool_runtime::ToolError::execution(
                    tool_id.clone(),
                    "DDG returned a bot challenge — search blocked after retries".to_string(),
                ));
            }

            return Ok(parse_ddg_html(&body));
        }

        // Unreachable (loop always returns or errors)
        Err(ds_tool_runtime::ToolError::execution(
            tool_id,
            "DDG search exhausted retries".to_string(),
        ))
    }
}

// ── HTML parsing ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse the DDG HTML results page.
///
/// Extracts result links (`class="result__a"`) and snippets
/// (`class="result__snippet"`). URLs are DDG redirect links with an
/// embedded `uddg` parameter containing the real URL (percent-encoded).
fn parse_ddg_html(body: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    // Extract result links: <a ... class="result__a" href="...">Title</a>
    let link_re = regex::Regex::new(
        r#"<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>([^<]+)</a>"#,
    );
    // Extract snippets: <a ... class="result__snippet">text</a>
    let snippet_re =
        regex::Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>([^<]*)</a>"#);

    let links: Vec<(String, String)> = if let Ok(re) = &link_re {
        re.captures_iter(body)
            .filter_map(|cap| {
                let href = cap.get(1)?.as_str().to_string();
                let title = cap.get(2)?.as_str().to_string();
                Some((href, title))
            })
            .collect()
    } else {
        Vec::new()
    };

    let snippets: Vec<String> = if let Ok(re) = &snippet_re {
        re.captures_iter(body)
            .filter_map(|cap| {
                let raw = cap.get(1)?.as_str();
                // Strip any nested HTML tags and decode entities
                let clean = strip_html_tags(raw);
                Some(clean)
            })
            .collect()
    } else {
        Vec::new()
    };

    // Pair links and snippets by position
    for (i, (href, title)) in links.iter().enumerate() {
        if i >= MAX_RESULTS {
            break;
        }
        let real_url = decode_ddg_url(href);
        if real_url.is_empty() || title.trim().is_empty() {
            continue;
        }
        if !seen_urls.insert(real_url.clone()) {
            continue;
        }
        let snippet = snippets.get(i).cloned().unwrap_or_default();
        results.push(SearchResult {
            title: title.trim().to_string(),
            url: real_url,
            snippet,
        });
    }

    results
}

/// Decode a DDG redirect URL (e.g. `//duckduckgo.com/l/?uddg=https%3A...`)
/// into the real destination URL.
fn decode_ddg_url(href: &str) -> String {
    // Pattern: //duckduckgo.com/l/?uddg=ENCODED_URL&rut=...
    if let Some(start) = href.find("uddg=") {
        let encoded = &href[start + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        let encoded_url = &encoded[..end];
        if let Ok(decoded) = url_decode(encoded_url) {
            return decoded;
        }
    }
    // If no uddg param, try using the href directly (strip protocol-relative prefix)
    if href.starts_with("//") {
        href.to_string()
    } else {
        href.to_string()
    }
}

/// Percent-decode a URL-encoded string.
fn url_decode(s: &str) -> Result<String, ()> {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                result.push((hi << 4 | lo) as char);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(' ');
            i += 1;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    Ok(result)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Strip HTML tags from a string and decode common entities.
fn strip_html_tags(s: &str) -> String {
    // Remove HTML tags
    let tag_re = regex::Regex::new(r"<[^>]+>");
    let without_tags = if let Ok(re) = &tag_re {
        re.replace_all(s, "").to_string()
    } else {
        s.to_string()
    };
    // Decode common HTML entities
    without_tags
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ── Helpers ────────────────────────────────────────────────────────────────

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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn backend_path_sends_responses_request_and_parses_output() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer enterprise-key"))
            .and(body_partial_json(serde_json::json!({
                "model": "enterprise-search",
                "tools": [{
                    "type": "web_search",
                    "web_search": {
                        "filters": { "allowed_domains": ["example.com"] }
                    }
                }],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_test",
                "object": "response",
                "created_at": 1,
                "model": "enterprise-search",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "search result",
                        "annotations": [{
                            "type": "url_citation",
                            "url": "https://example.com/result"
                        }]
                    }]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebSearchConfig::Enabled {
            api_key: "enterprise-key".into(),
            base_url: server.uri(),
            model: "enterprise-search".into(),
            extra_headers: Default::default(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None).unwrap();
        let (content, citations) = client
            .search(
                "test query",
                Some(vec!["example.com".to_string()]),
            )
            .await
            .unwrap();
        assert!(content.contains("search result"), "got: {content}");
        assert_eq!(citations, vec!["https://example.com/result"]);
    }

    #[tokio::test]
    async fn backend_path_reports_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebSearchConfig::Enabled {
            api_key: "bad-key".into(),
            base_url: server.uri(),
            model: "enterprise-search".into(),
            extra_headers: Default::default(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None).unwrap();
        // Test the backend leg directly (search() would fall back to DDG).
        let backend = client.backend.as_ref().expect("backend configured");
        let err = client
            .search_via_backend(backend, "test query", None)
            .await
            .expect_err("401 must surface from the backend path");
        assert!(
            err.to_string().contains("401"),
            "expected HTTP 401 in error, got: {err}"
        );
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(
            url_decode("https%3A%2F%2Frust-lang.org%2Flearn%2F").unwrap(),
            "https://rust-lang.org/learn/"
        );
        assert_eq!(url_decode("hello+world").unwrap(), "hello world");
        assert_eq!(url_decode("noencoding").unwrap(), "noencoding");
    }

    #[test]
    fn test_decode_ddg_url() {
        let href =
            "//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2Flearn%2F&rut=abc123";
        assert_eq!(decode_ddg_url(href), "https://rust-lang.org/learn/");
    }

    #[test]
    fn test_decode_ddg_url_no_uddg() {
        assert_eq!(decode_ddg_url("//example.com"), "//example.com");
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>Hello</b> World"), "Hello World");
        assert_eq!(
            strip_html_tags("foo &quot;bar&quot; &amp; baz"),
            "foo \"bar\" & baz"
        );
    }

    #[test]
    fn test_format_results() {
        let results = vec![SearchResult {
            title: "Rust".into(),
            url: "https://rust-lang.org".into(),
            snippet: "A systems language.".into(),
        }];
        let formatted = format_results(&results);
        assert!(formatted.contains("1. Rust"));
        assert!(formatted.contains("URL: https://rust-lang.org"));
        assert!(formatted.contains("A systems language."));
    }

    #[test]
    fn test_parse_ddg_html_empty() {
        let results = parse_ddg_html("");
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_ddg_html_with_results() {
        let body = r#"<html>
<body>
<div class="results">
<div class="result">
  <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc">Rust Programming Language</a>
  <a class="result__snippet">A systems programming language.</a>
</div>
<div class="result">
  <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FRust&amp;rut=def">Rust - Wikipedia</a>
  <a class="result__snippet">Rust is a general-purpose programming language.</a>
</div>
</div>
</body></html>"#;

        let results = parse_ddg_html(body);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(results[0].snippet, "A systems programming language.");
        assert_eq!(results[1].title, "Rust - Wikipedia");
        assert_eq!(results[1].url, "https://en.wikipedia.org/wiki/Rust");
    }

    #[test]
    fn test_parse_ddg_html_skips_duplicate_urls() {
        let body = r#"<html><body>
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&amp;rut=1">First</a>
<a class="result__snippet">Snippet 1</a>
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&amp;rut=2">Second</a>
<a class="result__snippet">Snippet 2</a>
</body></html>"#;

        let results = parse_ddg_html(body);
        assert_eq!(
            results.len(),
            1,
            "Duplicate URLs should be skipped"
        );
    }
}
