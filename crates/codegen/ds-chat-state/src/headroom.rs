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

    for item in conversation.iter_mut() {
        if remaining == 0 {
            break;
        }
        let ConversationItem::ToolResult(tr) = item else {
            continue;
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use ds_sampling_types::ConversationItem;

    #[test]
    fn request_clone_tool_results_compress() {
        ds_headroom::reset_for_test();
        set_enabled(true);
        ds_headroom::set_min_chars_override(50);
        ds_headroom::set_keep_lines_override(6);
        let big = (0..150)
            .map(|i| format!("L{i:04} {}", "ABCDEFGH".repeat(4)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut conv = vec![
            ConversationItem::user("go"),
            ConversationItem::tool_result("t1", big.clone()),
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
        assert_eq!(retrieve(hash).unwrap().content, big);
    }
}
