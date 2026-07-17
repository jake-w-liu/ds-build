//! Headroom integration for chat-state request building.
//!
//! Compresses large tool results on the *request clone* only. Re-exports the
//! shared store/toggle API from [`ds_headroom`].

use ds_sampling_types::ConversationItem;

pub use ds_headroom::{
    CompressionStats, ENV_HEADROOM, HEADROOM_RETRIEVE_TOOL_NAME, SessionStats, StoredContent,
    format_stats_report, is_enabled, retrieve, session_stats, set_enabled, store_stats,
};

/// Compress large tool results in a request clone (oldest first).
///
/// Skips coordination tools (subagent spawn/output, headroom_retrieve, …)
/// so agent orchestration dumps are not immediately re-compressed into
/// markers the model must retrieve again.
pub fn compress_tool_results(conversation: &mut [ConversationItem]) -> CompressionStats {
    let mut stats = CompressionStats::default();
    if !ds_headroom::is_enabled() {
        return stats;
    }
    let max_segments = std::env::var(ds_headroom::ENV_MAX_SEGMENTS)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(ds_headroom::DEFAULT_MAX_SEGMENTS);
    let mut remaining = max_segments;

    // Pair tool_call_id → tool name from preceding assistant tool_calls so we
    // can skip meta/coordination tools by name.
    let tool_names = tool_call_names(conversation);

    for item in conversation.iter_mut() {
        if remaining == 0 {
            break;
        }
        let ConversationItem::ToolResult(tr) = item else {
            continue;
        };
        if let Some(name) = tool_names.get(tr.tool_call_id.as_str())
            && ds_headroom::should_skip_tool_name(name)
        {
            continue;
        }
        let original = tr.content.as_ref();
        match ds_headroom::maybe_compress_content(original, Some(tr.tool_call_id.as_str()), &mut stats)
        {
            Some(compressed) => {
                tr.content = std::sync::Arc::<str>::from(compressed);
                remaining -= 1;
            }
            None => {}
        }
    }
    stats
}

/// Map `tool_call_id` → function name from assistant tool_calls in order.
fn tool_call_names(conversation: &[ConversationItem]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for item in conversation {
        let ConversationItem::Assistant(a) = item else {
            continue;
        };
        for tc in &a.tool_calls {
            map.insert(tc.id.as_ref().to_owned(), tc.name.clone());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_sampling_types::ConversationItem;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn big(n: usize) -> String {
        (0..n)
            .map(|i| format!("L{i:04} {}", "ABCDEFGH".repeat(4)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn request_clone_tool_results_compress() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ds_headroom::reset_for_test();
        set_enabled(true);
        ds_headroom::set_min_chars_override(50);
        ds_headroom::set_keep_lines_override(6);
        let content = big(150);
        let mut conv = vec![
            ConversationItem::user("go"),
            ConversationItem::tool_result("t1", content.clone()),
        ];
        let stats = compress_tool_results(&mut conv);
        assert!(stats.compressed_segments >= 1);
        assert!(stats.tokens_saved > 0);
        let ConversationItem::ToolResult(tr) = &conv[1] else {
            panic!();
        };
        assert!(tr.content.contains("<headroom_compressed"));
        let hash = tr
            .content
            .split("hash=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap();
        assert_eq!(retrieve(hash).unwrap().content, content);
    }

    #[test]
    fn skips_coordination_tool_results() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ds_headroom::reset_for_test();
        set_enabled(true);
        ds_headroom::set_min_chars_override(50);
        ds_headroom::set_keep_lines_override(6);
        let big_body = big(150);
        let tc = ds_sampling_types::ToolCall {
            id: std::sync::Arc::<str>::from("call-sub"),
            name: "spawn_subagent".into(),
            arguments: std::sync::Arc::<str>::from("{}"),
        };
        let tc2 = ds_sampling_types::ToolCall {
            id: std::sync::Arc::<str>::from("call-bash"),
            name: "run_terminal_command".into(),
            arguments: std::sync::Arc::<str>::from("{}"),
        };
        let mut conv = vec![
            ConversationItem::user("go"),
            ConversationItem::assistant_tool_calls(vec![tc, tc2]),
            ConversationItem::tool_result("call-sub", big_body.clone()),
            ConversationItem::tool_result("call-bash", big_body.clone()),
        ];
        let stats = compress_tool_results(&mut conv);
        // Subagent dump skipped; bash dump compressed.
        assert_eq!(stats.compressed_segments, 1);
        let ConversationItem::ToolResult(sub) = &conv[2] else {
            panic!();
        };
        assert!(
            !sub.content.contains("<headroom_compressed"),
            "spawn_subagent result must not be compressed"
        );
        assert_eq!(sub.content.as_ref(), big_body.as_str());
        let ConversationItem::ToolResult(bash) = &conv[3] else {
            panic!();
        };
        assert!(bash.content.contains("<headroom_compressed"));
    }

    #[test]
    fn respects_max_segments_env() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ds_headroom::reset_for_test();
        set_enabled(true);
        ds_headroom::set_min_chars_override(50);
        ds_headroom::set_keep_lines_override(6);
        // SAFETY: tests serialized via TEST_LOCK.
        unsafe {
            std::env::set_var(ds_headroom::ENV_MAX_SEGMENTS, "2");
        }
        let mut conv = vec![ConversationItem::user("go")];
        for i in 0..5 {
            conv.push(ConversationItem::tool_result(
                format!("t{i}"),
                big(120 + i),
            ));
        }
        let stats = compress_tool_results(&mut conv);
        unsafe {
            std::env::remove_var(ds_headroom::ENV_MAX_SEGMENTS);
        }
        assert_eq!(
            stats.compressed_segments, 2,
            "only first max_segments tool results should compress"
        );
        let mut compressed = 0u32;
        for item in &conv {
            if let ConversationItem::ToolResult(tr) = item
                && tr.content.contains("<headroom_compressed")
            {
                compressed += 1;
            }
        }
        assert_eq!(compressed, 2);
    }
}
