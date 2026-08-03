//! Correctness assertions over captured session artifacts.
//!
//! All checks drive the SHIPPED functions — no re-implementations:
//! - wire rules are asserted on the recorded request bodies, and the
//!   `reasoning_content` placement rule is additionally exercised through
//!   [`ds_sampling_types::conversation_to_chat_messages`];
//! - headroom markers are matched against the hash produced by
//!   [`ds_headroom::maybe_compress_content`], and byte-exactness via
//!   [`ds_headroom::retrieve`] / [`ds_headroom::retrieve_formatted`];
//! - the completion gate drives [`ds_tools::verification::completion::check_completion`].

use serde_json::Value;

use crate::mock::WireRequest;

/// Collect assertion failures as human-readable strings; empty = pass.
pub type Failures = Vec<String>;

// ---------------------------------------------------------------------------
// Wire rules
// ---------------------------------------------------------------------------

/// `reasoning_content` must be present on assistant tool_calls turns and
/// ABSENT on plain assistant turns; no message may carry an image part.
pub fn check_wire_reasoning_rules(wire: &[WireRequest]) -> Failures {
    let mut failures = Vec::new();
    for w in wire {
        if !w.path.ends_with("/chat/completions") {
            continue;
        }
        let Some(messages) = w.body.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for (i, msg) in messages.iter().enumerate() {
            // No image parts on the wire (text-only pipeline) — any role.
            if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                for part in content {
                    if let Some(t) = part.get("type").and_then(|t| t.as_str()) {
                        if t.contains("image") {
                            failures.push(format!(
                                "seq {}: image content part on the wire (text-only pipeline)",
                                w.seq
                            ));
                        }
                    }
                }
            }
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            let has_tool_calls = msg
                .get("tool_calls")
                .is_some_and(|v| !v.is_null());
            let rc = msg.get("reasoning_content");
            if has_tool_calls && rc.is_none() {
                failures.push(format!(
                    "seq {}: assistant tool_calls message [{i}] lacks reasoning_content (wire rule)",
                    w.seq
                ));
            }
            if !has_tool_calls && rc.is_some() {
                failures.push(format!(
                    "seq {}: plain assistant message [{i}] carries reasoning_content (wire rule)",
                    w.seq
                ));
            }
        }
    }
    failures
}

/// Extract the tool-call names on the last main-conversation request (debug
/// aid + web_search/spawn presence checks).
pub fn tool_names_on_last_request(wire: &[WireRequest], conv_id: &str) -> Vec<String> {
    let mut names = Vec::new();
    for w in wire.iter().rev() {
        if w.conv_id != conv_id || !w.path.ends_with("/chat/completions") {
            continue;
        }
        let Some(messages) = w.body.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for msg in messages {
            if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    if let Some(n) = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) {
                        names.push(n.to_string());
                    }
                }
            }
        }
        break;
    }
    names
}

/// Tool results (role=tool) present across all requests for a conversation.
pub fn tool_results_for_conv(wire: &[WireRequest], conv_id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for w in wire {
        if w.conv_id != conv_id || !w.path.ends_with("/chat/completions") {
            continue;
        }
        let Some(messages) = w.body.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for msg in messages {
            if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
                let content = match msg.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                out.push((w.req_id.clone(), content));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Headroom
// ---------------------------------------------------------------------------

/// Compress `original` with the shipped function; returns (marker, hash).
pub fn headroom_compress(original: &str) -> (String, Option<String>) {
    let mut stats = ds_headroom::CompressionStats::default();
    match ds_headroom::maybe_compress_content(original, Some("harness"), &mut stats) {
        Some(marker) => {
            let hash = marker
                .split("hash=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .map(str::to_string);
            (marker, hash)
        }
        None => (String::new(), None),
    }
}

/// Assert the wire's headroom behavior for a run: with `expect_marker ==
/// true` (headroom ON) a tool result must carry `<headroom_compressed
/// hash=…>` whose hash equals the shipped-function hash for
/// `expected_formatted`, and `retrieve` must return the byte-exact original.
/// With `expect_marker == false` (headroom OFF) no marker may appear and the
/// full content must be on the wire instead.
pub fn check_headroom_wire_and_roundtrip(
    wire: &[WireRequest],
    conv_id: &str,
    expected_formatted: &str,
    expect_marker: bool,
) -> Failures {
    let mut failures = Vec::new();
    let (_, expected_hash) = headroom_compress(expected_formatted);
    let Some(expected_hash) = expected_hash else {
        failures.push("harness-side headroom compression produced no marker (fixture too small?)".into());
        return failures;
    };

    let mut saw_expected_marker = false;
    let mut saw_any_marker = false;
    for (_, content) in tool_results_for_conv(wire, conv_id) {
        if content.contains("<headroom_compressed") {
            saw_any_marker = true;
            if content.contains(&expected_hash) {
                saw_expected_marker = true;
            }
        }
    }
    if expect_marker {
        if !saw_expected_marker {
            failures.push(format!(
                "no wire tool result carries <headroom_compressed hash=\"{expected_hash}\"> for conv {conv_id}"
            ));
        }
    } else if saw_any_marker {
        failures.push(format!(
            "headroom OFF run still compressed a tool result on the wire (hash {expected_hash})"
        ));
    }

    // Byte-exact round-trip through the shipped store functions.
    match ds_headroom::retrieve(&expected_hash) {
        Some(stored) => {
            if stored.content != expected_formatted {
                failures.push("headroom retrieve content != original (byte mismatch)".into());
            }
        }
        None => failures.push("headroom retrieve: hash not found in store".into()),
    }
    failures
}

/// ≥90% token reduction on the large results (acceptance 4): compress the
/// formatted fixture with the shipped function and check its stats.
pub fn check_compression_reduction(formatted: &str) -> Result<f64, String> {
    let mut stats = ds_headroom::CompressionStats::default();
    let marker = ds_headroom::maybe_compress_content(formatted, Some("harness"), &mut stats)
        .ok_or_else(|| "fixture did not compress".to_string())?;
    let _ = marker;
    if stats.tokens_before == 0 {
        return Err("compression stats: tokens_before == 0".into());
    }
    let reduction = 1.0 - (stats.tokens_after as f64 / stats.tokens_before as f64);
    Ok(reduction)
}

// ---------------------------------------------------------------------------
// Completion gate (shipped function)
// ---------------------------------------------------------------------------

/// Bare whole-task claim must be rejected; a sub-step claim must pass.
pub fn check_completion_gate() -> Failures {
    let mut failures = Vec::new();
    match ds_tools::verification::completion::check_completion("Done.") {
        Ok(()) => failures.push("completion gate: bare `Done.` was accepted".into()),
        Err(e) => {
            if !e.contains("CRITERION") {
                failures.push(format!("completion gate: `Done.` rejected but not for CRITERION: {e}"));
            }
        }
    }
    match ds_tools::verification::completion::check_completion("Build finished in 8m56s.") {
        Ok(()) => {}
        Err(e) => failures.push(format!(
            "completion gate: sub-step `Build finished.` was rejected: {e}"
        )),
    }
    // Evidence-backed completion accepted.
    match ds_tools::verification::completion::check_completion(
        "Done.\nCRITERION: fixture file exists\nOBSERVED: exit code: 0 from `test -f fixture_a.txt`",
    ) {
        Ok(()) => {}
        Err(e) => failures.push(format!("completion gate: evidence-backed claim rejected: {e}")),
    }
    failures
}

// ---------------------------------------------------------------------------
// conversation_to_chat_messages (shipped function) — reasoning placement
// ---------------------------------------------------------------------------

/// Exercise the shipped serializer on a conversation with a reasoning-bearing
/// tool_calls turn: reasoning must land on the tool_calls message only.
pub fn check_conversation_to_chat_messages_reasoning_rule() -> Failures {
    use ds_sampling_types::{ConversationItem, Role, ToolCall};
    let mut failures = Vec::new();
    let tc = ToolCall {
        id: std::sync::Arc::<str>::from("call-1"),
        name: "read_file".into(),
        arguments: std::sync::Arc::<str>::from("{}"),
    };
    let conv = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("read it"),
        ConversationItem::Reasoning(ds_sampling_types::rs::ReasoningItem {
            id: "reason-1".into(),
            summary: vec![],
            content: Some(vec![ds_sampling_types::rs::ReasoningTextContent {
                text: "thinking about the file".into(),
            }]),
            encrypted_content: None,
            status: None,
        }),
        ConversationItem::assistant_tool_calls(vec![tc]),
        ConversationItem::tool_result("call-1", "content"),
        ConversationItem::assistant("plain turn without tools"),
    ];
    let msgs = ds_sampling_types::conversation_to_chat_messages(conv);
    let mut saw_tool_calls_rc = false;
    for m in &msgs {
        let is_assistant = m.role == Role::Assistant;
        let has_tc = !m.tool_calls.is_empty();
        let has_rc = m.reasoning_content.is_some();
        if is_assistant && has_tc {
            if !has_rc {
                failures.push("shipped serializer: tool_calls turn lost reasoning_content".into());
            }
            saw_tool_calls_rc = true;
        }
        if is_assistant && !has_tc && has_rc {
            failures.push("shipped serializer: plain assistant turn carries reasoning_content".into());
        }
    }
    if !saw_tool_calls_rc {
        failures.push("shipped serializer: no tool_calls message observed".into());
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(seq: u64, conv: &str, body: Value) -> WireRequest {
        WireRequest {
            seq,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            conv_id: conv.into(),
            req_id: format!("r{seq}"),
            authorization: None,
            body,
            status: 200,
            response_preview: String::new(),
        }
    }

    #[test]
    fn wire_rule_accepts_correct_and_rejects_violations() {
        // Correct: tool_calls msg with reasoning, plain msg without.
        let good = req(
            1,
            "c",
            json!({"messages": [
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1"}],
                 "reasoning_content": "thinking"},
                {"role": "assistant", "content": "plain"}
            ]}),
        );
        assert!(check_wire_reasoning_rules(&[good]).is_empty());

        // Violation 1: tool_calls without reasoning_content.
        let bad1 = req(
            2,
            "c",
            json!({"messages": [
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1"}]}
            ]}),
        );
        let f = check_wire_reasoning_rules(&[bad1]);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("lacks reasoning_content"));

        // Violation 2: plain assistant with reasoning_content.
        let bad2 = req(
            3,
            "c",
            json!({"messages": [
                {"role": "assistant", "content": "x", "reasoning_content": "y"}
            ]}),
        );
        let f = check_wire_reasoning_rules(&[bad2]);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("plain assistant"));

        // Violation 3: image part on the wire.
        let bad3 = req(
            4,
            "c",
            json!({"messages": [
                {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "x"}}]}
            ]}),
        );
        let f = check_wire_reasoning_rules(&[bad3]);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("image"));
    }

    #[test]
    fn headroom_marker_hash_matches_and_roundtrip_is_byte_exact() {
        let content = (0..300)
            .map(|i| format!("LINE{i:04} abcdefghijklmnopqrstuvwxyz0123456789"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, hash) = headroom_compress(&content);
        let hash = hash.expect("compressed");
        let wire = vec![req(
            1,
            "c",
            json!({"messages": [
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1"}]},
                {"role": "tool", "tool_call_id": "c1",
                 "content": format!("<headroom_compressed hash=\"{hash}\" original_chars=12000 compressed_chars=500 tokens_before=3000 tokens_after=125/>")}
            ]}),
        )];
        let failures = check_headroom_wire_and_roundtrip(&wire, "c", &content, true);
        assert!(failures.is_empty(), "{failures:?}");
        // OFF-mode: full content, no marker.
        let wire_off = vec![req(
            2,
            "c",
            json!({"messages": [
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1"}]},
                {"role": "tool", "tool_call_id": "c1", "content": content}
            ]}),
        )];
        let failures = check_headroom_wire_and_roundtrip(&wire_off, "c", &content, false);
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn compression_reduction_exceeds_90pct() {
        let content = (0..800)
            .map(|i| format!("LINE{i:04} aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd eeeeeeeeee"))
            .collect::<Vec<_>>()
            .join("\n");
        let reduction = check_compression_reduction(&content).expect("compresses");
        assert!(
            reduction >= 0.90,
            "reduction={reduction} — headroom must cut ≥90% tokens on the big fixture"
        );
    }

    #[test]
    fn completion_gate_behavior_via_shipped_fn() {
        assert!(check_completion_gate().is_empty());
    }

    #[test]
    fn shipped_serializer_places_reasoning_on_tool_calls_only() {
        assert!(check_conversation_to_chat_messages_reasoning_rule().is_empty());
    }
}
