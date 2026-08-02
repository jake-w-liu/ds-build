//! Wire-level regression: the non-streaming Responses path must inject
//! DeepSeek-specific raw tools (e.g. `x_search`) into the serialized body
//! exactly like the streaming path does, and the base `tools` array must be
//! alphabetized (DeepSeek automatic prefix-cache byte stability).

use std::sync::{Arc, Mutex};

use axum::extract::Json;
use axum::routing::post;
use axum::Router;
use ds_sampler::{ApiBackend, SamplerConfig, SamplingClient};
use ds_sampling_types::{ConversationItem, ConversationRequest, HostedTool, ToolSpec};

fn test_client(base_url: &str) -> SamplingClient {
    SamplingClient::new(SamplerConfig {
        api_key: Some("test-key".to_string()),
        base_url: base_url.to_string(),
        model: "deepseek-v4-pro".to_string(),
        api_backend: ApiBackend::Responses,
        ..Default::default()
    })
    .expect("client should construct")
}

/// Minimal non-streaming `rs::Response` body that deserializes successfully.
fn minimal_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp_test_1",
        "object": "response",
        "created_at": 1,
        "model": "deepseek-v4-pro",
        "status": "completed",
        "output": [],
    })
}

#[tokio::test]
async fn non_streaming_responses_injects_x_search_and_sorts_function_tools() {
    let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = bodies.clone();
    let app = Router::new().route(
        "/responses",
        post(move |Json(body): Json<serde_json::Value>| {
            capture.lock().unwrap().push(body);
            async move { Json(minimal_response()) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = test_client(&format!("http://{addr}"));
    let mut request =
        ConversationRequest::from_items(vec![ConversationItem::user("find files")]).with_tools(vec![
            ToolSpec {
                name: "zz_last".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSpec {
                name: "aa_first".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
            },
        ]);
    request.hosted_tools = vec![HostedTool::XSearch];

    let response = client.conversation_responses(request).await.expect("request");
    assert_eq!(response.id, "resp_test_1");

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "exactly one request captured");
    let body = &bodies[0];
    let tools = body
        .get("tools")
        .and_then(|v| v.as_array())
        .expect("tools array on the wire");

    let names: Vec<&str> = tools
        .iter()
        .map(|t| t.get("name").and_then(|n| n.as_str()).unwrap_or(t.get("type").and_then(|ty| ty.as_str()).unwrap_or("?")))
        .collect();
    assert_eq!(
        names,
        ["aa_first", "zz_last", "x_search"],
        "function tools sorted; raw x_search appended last"
    );
}
