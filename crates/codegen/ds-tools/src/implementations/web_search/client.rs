//! Web search client — DuckDuckGo HTML scraping.
//!
//! No API key required. Scrapes the DDG HTML (non-JS) search endpoint,
//! which returns plain HTML results. Uses `native-tls` (OS TLS stack)
//! so the TLS fingerprint matches a real browser — avoids bot detection
//! that blocks `rustls`-based clients.
//!
//! The DDG Instant Answer API (`api.duckduckgo.com`) is intentionally NOT
//! used: it only returns structured data for dictionary/definition queries,
//! not general web search, and is now bot-blocked from most IPs.
//!
//! Rate limit: the HTML endpoint is tolerant of moderate usage but may
//! challenge excessive request rates. Keep queries spaced reasonably.

use super::types::WebSearchConfig;
use crate::attribution::SharedAttributionCallback;
use crate::types::SharedApiKeyProvider;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

/// DuckDuckGo HTML search endpoint (non-JS version).
const DDG_HTML_URL: &str = "https://html.duckduckgo.com/html/";

/// Maximum search results.
const MAX_RESULTS: usize = 10;

/// Delay before DDG requests to avoid rate limiting (milliseconds).
const DDG_REQUEST_DELAY_MS: u64 = 200;

/// Max retries on rate-limit / bot-challenge responses.
const DDG_MAX_RETRIES: usize = 2;

/// Backoff multiplier (seconds) between retries.
const DDG_RETRY_BACKOFF_SECS: u64 = 2;

/// HTTP client that performs web searches via DuckDuckGo HTML scraping.
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
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

        Ok(Self { http })
    }

    pub fn with_attribution_callback(
        self,
        _callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self
    }

    /// Perform a web search via DuckDuckGo HTML scraping.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), ds_tool_runtime::ToolError> {
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

    /// Search via DuckDuckGo HTML endpoint.
    ///
    /// The HTML endpoint returns a simple non-JS page with result links
    /// (`class="result__a"`) and snippets (`class="result__snippet"`).
    /// URLs are DDG redirect links; we extract and decode the `uddg` param
    /// to get the real destination URL.
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

        let url = format!("{}?q={}", DDG_HTML_URL, url_encode(&q));
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
                .get(&url)
                .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
                .header("Accept-Language", "en-US,en;q=0.9")
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
    let snippet_re = regex::Regex::new(
        r#"<a[^>]*class="result__snippet"[^>]*>((?:(?!</a>).)*)</a>"#,
    );

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

fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            b' ' => result.push('+'),
            _ => {
                result.push('%');
                result.push(hex_char(b >> 4));
                result.push(hex_char(b & 0x0f));
            }
        }
    }
    result
}

fn hex_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
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

// ── Tests ──────────────────────────────────────────────────────────────────

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
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2Flearn%2F&rut=abc123";
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

    #[tokio::test]
    #[ignore = "live HTTP test — run with `cargo test -- --ignored`"]
    async fn test_live_ddg_html_search() {
        let config = WebSearchConfig::Enabled {
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            extra_headers: indexmap::IndexMap::new(),
            alpha_test_key: None,
        };
        let client = WebSearchClient::new(&config, None).expect("create client");
        let (content, citations) = client.search("DeepSeek R1 reasoning", None).await.expect("search");
        eprintln!("CONTENT:\n{content}");
        assert!(!citations.is_empty(), "must return at least one citation");
        assert!(!content.contains("No search results found"), "must have real results");
        assert!(citations.iter().any(|c| c.starts_with("https://")), "must have HTTPS URLs");
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
        assert_eq!(results.len(), 1, "Duplicate URLs should be skipped");
    }
}
