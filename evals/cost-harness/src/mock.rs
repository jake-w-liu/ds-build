//! Recording HTTP server: scripted mock in mock mode, forward-proxy in live
//! mode. Writes the mode-independent capture files:
//! - `wire.jsonl` — every request (headers incl. x-ds-conv-id / x-ds-req-id,
//!   full body) + response status/preview;
//! - `usage.jsonl` — one [`UsageRow`] per model request.
//!
//! Mock behavior for `/v1/chat/completions`:
//! - title requests (`tool_choice.function.name == "session_title"`) → text;
//! - `ds-compact-*` / `ds-recap-*` requests → `ok` text;
//! - main conversation (`x-ds-conv-id` == the harness session id) → next item
//!   from the scenario script (tool_calls stream with reasoning_content, or
//!   plain text);
//! - anything else (subagent conversations) → `ok` text.
//! - `/v1/responses` → HTTP 400 (forces the web_search DuckDuckGo fallback);
//! - `/v1/models` → a small catalog; everything else → 404.
//!
//! Usage in mock mode is derived from the request body the server actually
//! received (`chars/4`), so headroom ON (markers, small body) vs OFF (full
//! content, big body) shows up as real, deterministic cost differences. Live
//! mode parses the real gateway usage object from the relayed SSE stream —
//! same capture format, same cost math.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, oneshot};

use crate::scenarios::MockItem;
use crate::usage::UsageRow;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Scripted model behavior for the main conversation.
    pub script: Vec<MockItem>,
    /// The harness's session id (main conversation key).
    pub main_conv_id: String,
    /// Directory for wire.jsonl / usage.jsonl.
    pub out_dir: PathBuf,
    /// Live mode: forward model traffic to this upstream base URL.
    pub forward: Option<String>,
}

pub struct RecordingServer {
    pub addr: SocketAddr,
    pub wire_path: PathBuf,
    pub usage_path: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// One recorded request (wire.jsonl row).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WireRequest {
    pub seq: u64,
    pub method: String,
    pub path: String,
    pub conv_id: String,
    pub req_id: String,
    pub authorization: Option<String>,
    pub body: Value,
    pub status: u16,
    pub response_preview: String,
}

#[derive(Default)]
struct Shared {
    seq: u64,
}

/// Main-conversation script consumption (one harness mock at a time).
static CONSUMED: std::sync::Mutex<usize> = std::sync::Mutex::new(0);

/// Reset the script cursor before starting a new scenario run.
pub fn reset_script_consumption() {
    *CONSUMED.lock().unwrap_or_else(|e| e.into_inner()) = 0;
}

impl RecordingServer {
    pub async fn start(cfg: ServerConfig) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind recording server")?;
        let addr = listener.local_addr().context("recording addr")?;
        let wire_path = cfg.out_dir.join("wire.jsonl");
        let usage_path = cfg.out_dir.join("usage.jsonl");
        let shared = Arc::new(RwLock::new(Shared::default()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let shared_task = shared.clone();
        let cfg_task = cfg.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { continue };
                        let shared = shared_task.clone();
                        let cfg = cfg_task.clone();
                        tokio::spawn(async move {
                            let _ = serve_conn(stream, shared, cfg).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            addr,
            wire_path,
            usage_path,
            shutdown: Some(shutdown_tx),
            handle: Some(handle),
        })
    }

    /// Stop the server; records are read back from the capture files.
    pub async fn stop(&mut self) -> Result<(Vec<WireRequest>, Vec<UsageRow>)> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
        Ok((read_wire(&self.wire_path)?, read_usage(&self.usage_path)?))
    }
}

/// Read wire records from the capture file (stable across modes).
pub fn read_wire(path: &std::path::Path) -> Result<Vec<WireRequest>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).context("wire.jsonl row")?);
    }
    Ok(out)
}

/// Read usage rows from the capture file (stable across modes).
pub fn read_usage(path: &std::path::Path) -> Result<Vec<UsageRow>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).context("usage.jsonl row")?);
    }
    Ok(out)
}

/// Append a wire row + usage row to the capture files.
fn persist(out_dir: &std::path::Path, wire: &WireRequest, row: Option<&UsageRow>) -> Result<()> {
    use std::io::Write;
    let mut w = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("wire.jsonl"))?;
    writeln!(w, "{}", serde_json::to_string(wire)?)?;
    if let Some(row) = row {
        let mut u = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out_dir.join("usage.jsonl"))?;
        writeln!(u, "{}", serde_json::to_string(row)?)?;
    }
    Ok(())
}

async fn serve_conn(
    mut stream: tokio::net::TcpStream,
    shared: Arc<RwLock<Shared>>,
    cfg: ServerConfig,
) -> Result<()> {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut read_buf = [0u8; 16 * 1024];
    loop {
        // Read until the header terminator.
        let header_end = loop {
            if let Some(e) = find_subslice(&buf, b"\r\n\r\n") {
                break e;
            }
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                stream.read(&mut read_buf),
            )
            .await
            .context("read timeout")??;
            if n == 0 {
                return Ok(()); // client closed the connection
            }
            buf.extend_from_slice(&read_buf[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        let mut conv_id = String::new();
        let mut req_id = String::new();
        let mut authorization = String::new();
        for line in lines {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = lower.strip_prefix("x-ds-conv-id:") {
                conv_id = v.trim().to_string();
            } else if let Some(v) = lower.strip_prefix("x-ds-req-id:") {
                req_id = v.trim().to_string();
            } else if lower.starts_with("authorization:") {
                // Keep the VALUE's original case ("Bearer sk-…") — the
                // gateway rejects a lowercased scheme.
                authorization = line["authorization:".len()..].trim().to_string();
            }
        }
        // Read the body.
        let body_start = header_end + 4;
        while buf.len() < body_start + content_length {
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                stream.read(&mut read_buf),
            )
            .await
            .context("body read timeout")??;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&read_buf[..n]);
        }
        let body_bytes = buf[body_start..body_start + content_length].to_vec();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        buf.drain(..body_start + content_length);

        let body: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);

        let (status, response_body, preview) = if let Some(upstream) = cfg.forward.clone() {
            forward_request(&upstream, &method, &path, &body_str, &authorization).await
        } else {
            mock_request(&cfg, &method, &path, &body, &body_str, &conv_id, &req_id).await
        };

        let seq = {
            let mut g = shared.write().await;
            g.seq += 1;
            g.seq
        };
        let wire = WireRequest {
            seq,
            method,
            path,
            conv_id,
            req_id,
            authorization: (!authorization.is_empty()).then_some(authorization),
            body,
            status,
            response_preview: preview,
        };
        let row = usage_from_response(&wire, &response_body);
        persist(&cfg.out_dir, &wire, row.as_ref())
            .context("persist capture files")?;

        let resp = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            status_text(status),
            if wire.path.ends_with("/chat/completions") {
                "text/event-stream"
            } else {
                "application/json"
            },
            response_body.len(),
            response_body,
        );
        stream.write_all(resp.as_bytes()).await?;
        stream.flush().await?;
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Status",
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Mock-mode request handling (scripted, deterministic).
async fn mock_request(
    cfg: &ServerConfig,
    method: &str,
    path: &str,
    body: &Value,
    body_str: &str,
    conv_id: &str,
    req_id: &str,
) -> (u16, String, String) {
    if path.ends_with("/responses") {
        // web_search backend: force the DuckDuckGo fallback deterministically.
        return (
            400,
            "{\"error\":{\"message\":\"harness mock: backend search unavailable\"}}".into(),
            "400 backend search unavailable".into(),
        );
    }
    if path.ends_with("/models") && method == "GET" {
        return (
            200,
            json!({"object":"list","data":[
                {"id":"deepseek-v4-flash","object":"model","owned_by":"deepseek"},
                {"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek"}
            ]})
            .to_string(),
            "models catalog".into(),
        );
    }
    if !path.ends_with("/chat/completions") {
        return (404, "{\"error\":\"not found\"}".into(), "404".into());
    }

    let is_title = body.pointer("/tool_choice/function/name").and_then(|v| v.as_str())
        == Some("session_title");
    let is_compact = req_id.starts_with("ds-compact-");
    let is_recap = req_id.starts_with("ds-recap-");
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("deepseek-v4-flash")
        .to_string();

    let (response_text, reasoning, tool_calls, finish) = if is_title {
        ("Session title".to_string(), String::new(), None::<Value>, "stop")
    } else if is_compact {
        // Compaction summary requests expect a <summary>…</summary> block.
        ("<summary>ok</summary>".to_string(), String::new(), None::<Value>, "stop")
    } else if is_recap {
        ("ok".to_string(), String::new(), None::<Value>, "stop")
    } else if conv_id == cfg.main_conv_id {
        let idx = {
            let mut g = CONSUMED.lock().unwrap_or_else(|e| e.into_inner());
            let i = *g;
            *g += 1;
            i
        };
        match cfg.script.get(idx) {
            Some(MockItem::Tool { name, args }) => {
                let call_id = format!("call_h_{idx}");
                (
                    String::new(),
                    format!("reasoning step {idx}: plan the tool use"),
                    Some(json!([{
                        "index": 0,
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": args.to_string() }
                    }])),
                    "tool_calls",
                )
            }
            Some(MockItem::Text { text }) => (text.clone(), String::new(), None::<Value>, "stop"),
            None => ("ok".to_string(), String::new(), None::<Value>, "stop"),
        }
    } else {
        ("ok".to_string(), String::new(), None::<Value>, "stop")
    };

    // Mock usage: prompt tokens = actual received body (chars/4) so headroom
    // ON (markers) vs OFF (full content) is visible in cost.
    let prompt_tokens = (body_str.chars().count() / 4) as u64;
    let stream = build_sse_chunks(
        &model,
        &response_text,
        &reasoning,
        tool_calls.as_ref(),
        finish,
        prompt_tokens,
    );
    let preview = format!("{finish}: {}", truncate(&response_text, 60));
    (200, stream, preview)
}

fn build_sse_chunks(
    model: &str,
    text: &str,
    reasoning: &str,
    tool_calls: Option<&Value>,
    finish: &str,
    prompt_tokens: u64,
) -> String {
    let id = "chatcmpl-harness-1";
    let created = 1_700_000_000;
    let mut out = String::new();
    let mut delta = json!({"role": "assistant"});
    if !reasoning.is_empty() {
        delta["reasoning_content"] = Value::String(reasoning.to_string());
    }
    if let Some(tc) = tool_calls {
        delta["tool_calls"] = tc.clone();
        delta["content"] = Value::Null;
    } else {
        delta["content"] = Value::String(text.to_string());
    }
    out.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
        })
    ));
    let completion_tokens = ((text.chars().count() + reasoning.chars().count()) / 4).max(1) as u64;
    let reasoning_tokens = (reasoning.chars().count() / 4) as u64;
    out.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
                "prompt_tokens_details": {"cached_tokens": 0},
                "completion_tokens_details": {"reasoning_tokens": reasoning_tokens}
            }
        })
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Derive the usage row for a request from its response body: mock mode reads
/// the body-derived chunk the mock served; live mode parses the real upstream
/// usage object from the relayed SSE stream. `None` for non-inference routes.
fn usage_from_response(wire: &WireRequest, response_body: &str) -> Option<UsageRow> {
    if !wire.path.ends_with("/chat/completions") {
        return None;
    }
    let model = wire
        .body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("deepseek-v4-flash")
        .to_string();
    let mut usage: Option<Value> = None;
    for line in response_body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v.get("usage").is_some() {
                    usage = Some(v["usage"].clone());
                }
            }
        }
    }
    let u = usage?;
    let prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
    let completion_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    let reasoning = u["completion_tokens_details"]["reasoning_tokens"]
        .as_u64()
        .unwrap_or(0);
    Some(UsageRow {
        seq: wire.seq,
        conv_id: wire.conv_id.clone(),
        req_id: wire.req_id.clone(),
        model,
        input_tokens: prompt_tokens,
        cache_read_tokens: cached,
        output_tokens: completion_tokens,
        reasoning_tokens: reasoning,
        total_tokens: prompt_tokens + completion_tokens,
    })
}

/// Live mode: relay the request upstream and return the raw response body.
async fn forward_request(
    upstream: &str,
    method: &str,
    path: &str,
    body: &str,
    authorization: &str,
) -> (u16, String, String) {
    let url = format!("{}{}", upstream.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap();
    let mut req = if method == "GET" {
        client.get(&url)
    } else {
        client.post(&url).body(body.to_string())
    };
    if !authorization.is_empty() {
        req = req.header("authorization", authorization);
    }
    let resp = match req.header("content-type", "application/json").send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                502,
                format!("{{\"error\":\"forward failed: {e}\"}}"),
                format!("forward failed: {e}"),
            )
        }
    };
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let preview = truncate(&text, 80);
    (status, text, preview)
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(body: Value, path: &str) -> WireRequest {
        WireRequest {
            seq: 1,
            method: "POST".into(),
            path: path.into(),
            conv_id: "c".into(),
            req_id: "r".into(),
            authorization: None,
            body,
            status: 200,
            response_preview: String::new(),
        }
    }

    #[test]
    fn sse_usage_parses_text_and_tool_streams() {
        let stream = build_sse_chunks(
            "deepseek-v4-flash",
            "ROUND_TRIP_DONE",
            "",
            None,
            "stop",
            1000,
        );
        let row = usage_from_response(&wire(json!({"model": "deepseek-v4-flash"}), "/v1/chat/completions"), &stream).unwrap();
        assert_eq!(row.model, "deepseek-v4-flash");
        assert_eq!(row.input_tokens, 1000);
        assert_eq!(row.cache_read_tokens, 0);
        assert_eq!(row.output_tokens, ("ROUND_TRIP_DONE".chars().count() / 4).max(1) as u64);
        assert_eq!(row.reasoning_tokens, 0);

        let tc = json!([{"index":0,"id":"call_h_0","type":"function","function":{"name":"read_file","arguments":"{}"}}]);
        let stream2 = build_sse_chunks("deepseek-v4-flash", "", "reasoning xyz", Some(&tc), "tool_calls", 500);
        let row2 = usage_from_response(&wire(json!({"model": "deepseek-v4-flash"}), "/v1/chat/completions"), &stream2).unwrap();
        assert!(row2.reasoning_tokens >= 1);
        assert!(row2.output_tokens >= row2.reasoning_tokens);
    }

    #[test]
    fn non_inference_routes_have_no_usage_row() {
        let row = usage_from_response(&wire(json!({}), "/v1/responses"), "{}");
        assert!(row.is_none());
    }
}
