//! Local Headroom compression for large tool results.
//!
//! When enabled (`DS_HEADROOM=1` or [`set_enabled`]), large tool-result bodies
//! in the *request clone* are replaced with a short preview + hash marker.
//! Exact originals stay in a process-local store and can be pulled back via the
//! `headroom_retrieve` tool. Conversation state on disk / in the actor is not
//! mutated — only the outbound `ConversationRequest` items.
//!
//! Ported from cloud-code's built-in Headroom (preview + store + retrieve).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_token_estimation::estimate_tokens;
use sha2::{Digest, Sha256};

/// Env: enable Headroom for this process (`1`/`true`/`on`).
pub const ENV_HEADROOM: &str = "DS_HEADROOM";
/// Env: minimum tool-result size (chars) before compression is attempted.
pub const ENV_MIN_CHARS: &str = "DS_HEADROOM_MIN_CHARS";
/// Env: max tool results compressed per request.
pub const ENV_MAX_SEGMENTS: &str = "DS_HEADROOM_MAX_SEGMENTS";
/// Env: lines kept (split head/tail) for plain-text previews.
pub const ENV_KEEP_LINES: &str = "DS_HEADROOM_KEEP_LINES";
/// Env: max original store entries.
pub const ENV_MAX_STORE_ENTRIES: &str = "DS_HEADROOM_MAX_STORE_ENTRIES";
/// Env: max total stored original chars.
pub const ENV_MAX_STORE_CHARS: &str = "DS_HEADROOM_MAX_STORE_CHARS";

const DEFAULT_MIN_CHARS: usize = 2_000;
pub const DEFAULT_MAX_SEGMENTS: usize = 12;
const DEFAULT_KEEP_LINES: usize = 40;
const DEFAULT_MAX_STORE_ENTRIES: usize = 256;
const DEFAULT_MAX_STORE_CHARS: usize = 16 * 1024 * 1024;

/// Process override for min chars (0 = use env/default). Used by tests.
static MIN_CHARS_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static KEEP_LINES_OVERRIDE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
const MAX_SUMMARY_CHARS: usize = 8_000;
const MAX_JSON_PREVIEW_INPUT: usize = 1024 * 1024;
const DEFAULT_JSON_ITEMS: usize = 8;

/// Tool name for exact original retrieval (must match the registered tool id).
pub const HEADROOM_RETRIEVE_TOOL_NAME: &str = "headroom_retrieve";

static ENABLED_OVERRIDE: AtomicBool = AtomicBool::new(false);
static ENABLED_SET: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub attempted_segments: u32,
    pub compressed_segments: u32,
    pub failed_segments: u32,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
    pub hashes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StoredContent {
    pub hash: String,
    pub content: String,
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub attempted_segments: u32,
    pub compressed_segments: u32,
    pub failed_segments: u32,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
    pub last_error: Option<String>,
}

struct Store {
    entries: HashMap<String, StoredContent>,
    order: Vec<String>,
    total_chars: usize,
    session: SessionStats,
}

impl Store {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            total_chars: 0,
            session: SessionStats::default(),
        }
    }
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut guard = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(Store::new());
    }
    f(guard.as_mut().expect("store just initialized"))
}

/// Whether Headroom is currently enabled.
pub fn is_enabled() -> bool {
    if ENABLED_SET.load(Ordering::Relaxed) {
        return ENABLED_OVERRIDE.load(Ordering::Relaxed);
    }
    env_truthy(ENV_HEADROOM)
}

/// Force enable/disable for this process (slash `/headroom on|off`).
pub fn set_enabled(enabled: bool) {
    ENABLED_SET.store(true, Ordering::Relaxed);
    ENABLED_OVERRIDE.store(enabled, Ordering::Relaxed);
    if enabled {
        // Also set env so child subagents inherit when possible.
        // SAFETY: single-threaded toggle from slash command / tests; process-wide flag.
        unsafe {
            std::env::set_var(ENV_HEADROOM, "1");
        }
    } else {
        unsafe {
            std::env::remove_var(ENV_HEADROOM);
        }
    }
}

/// Session-wide compression counters (since process start / last reset).
pub fn session_stats() -> SessionStats {
    with_store(|s| s.session.clone())
}

/// Store occupancy.
pub fn store_stats() -> (usize, usize, usize, usize) {
    with_store(|s| {
        (
            s.entries.len(),
            max_store_entries(),
            s.total_chars,
            max_store_chars(),
        )
    })
}

/// Retrieve an original by hash (hex, case-insensitive).
pub fn retrieve(hash: &str) -> Option<StoredContent> {
    let key = normalize_hash(hash)?;
    with_store(|s| s.entries.get(&key).cloned())
}

/// Reset store + stats (tests / live harness).
pub fn reset_for_test() {
    ENABLED_SET.store(false, Ordering::Relaxed);
    ENABLED_OVERRIDE.store(false, Ordering::Relaxed);
    MIN_CHARS_OVERRIDE.store(0, Ordering::Relaxed);
    KEEP_LINES_OVERRIDE.store(0, Ordering::Relaxed);
    with_store(|s| {
        *s = Store::new();
    });
}

/// Test/live harness: force min-char threshold (0 clears override).
pub fn set_min_chars_override(n: usize) {
    MIN_CHARS_OVERRIDE.store(n, Ordering::Relaxed);
}

/// Test/live harness: force keep-lines (0 clears override).
pub fn set_keep_lines_override(n: usize) {
    KEEP_LINES_OVERRIDE.store(n, Ordering::Relaxed);
}

/// Compress a single large tool-result body if it qualifies.
/// Returns `Some(compressed)` when stored and smaller; `None` otherwise.
pub fn maybe_compress_content(
    text: &str,
    tool_call_id: Option<&str>,
    stats: &mut CompressionStats,
) -> Option<String> {
    if !is_enabled() {
        return None;
    }
    let min_chars = min_chars();
    if text.len() < min_chars || is_protected_content(text) {
        return None;
    }
    compress_one(text, tool_call_id, stats)
}

fn min_chars() -> usize {
    let o = MIN_CHARS_OVERRIDE.load(Ordering::Relaxed);
    if o > 0 {
        return o;
    }
    positive_usize_env(ENV_MIN_CHARS, DEFAULT_MIN_CHARS, 1_000_000)
}

fn keep_lines() -> usize {
    let o = KEEP_LINES_OVERRIDE.load(Ordering::Relaxed);
    if o > 0 {
        return o;
    }
    positive_usize_env(ENV_KEEP_LINES, DEFAULT_KEEP_LINES, 1000)
}


fn compress_one(
    text: &str,
    tool_call_id: Option<&str>,
    stats: &mut CompressionStats,
) -> Option<String> {
    stats.attempted_segments += 1;
    with_store(|s| s.session.attempted_segments += 1);

    if text.len() > max_store_chars() {
        return None;
    }

    let before = estimate_tokens(text);
    let hash = hash_content(text);
    let compressed = build_compressed_text(text, &hash, tool_call_id);
    if compressed == text {
        return None;
    }
    let after = estimate_tokens(&compressed);
    if after >= before || compressed.len() >= text.len() {
        return None;
    }

    let remembered = remember(StoredContent {
        hash: hash.clone(),
        content: text.to_string(),
        original_chars: text.len(),
        compressed_chars: compressed.len(),
        tool_call_id: tool_call_id.map(str::to_owned),
    });
    if !remembered {
        return None;
    }

    let saved = before.saturating_sub(after);
    stats.compressed_segments += 1;
    stats.tokens_before += before;
    stats.tokens_after += after;
    stats.tokens_saved += saved;
    stats.hashes.push(hash.clone());

    with_store(|s| {
        s.session.compressed_segments += 1;
        s.session.tokens_before += before;
        s.session.tokens_after += after;
        s.session.tokens_saved += saved;
    });

    tracing::info!(
        target: "ds_headroom",
        hash = %hash,
        original_chars = text.len(),
        compressed_chars = compressed.len(),
        tokens_before = before,
        tokens_after = after,
        tokens_saved = saved,
        "headroom compressed tool result"
    );

    Some(compressed)
}

fn build_compressed_text(text: &str, hash: &str, tool_call_id: Option<&str>) -> String {
    let Some(summary) = summarize_json(text).or_else(|| summarize_plain_text(text)) else {
        return text.to_string();
    };
    let summary = truncate_chars(&summary, MAX_SUMMARY_CHARS);
    let tool_attr = tool_call_id
        .map(|id| format!(" tool_call_id=\"{}\"", escape_attr(id)))
        .unwrap_or_default();
    format!(
        "<headroom_compressed hash=\"{hash}\" original_chars=\"{orig}\"{tool_attr}>\n\
         Original content is stored in this DS session. Use the `{tool}` tool with hash \"{hash}\" if exact content is needed.\n\n\
         {summary}\n\
         </headroom_compressed>",
        orig = text.len(),
        tool = HEADROOM_RETRIEVE_TOOL_NAME,
    )
}

fn summarize_json(text: &str) -> Option<String> {
    if text.len() > MAX_JSON_PREVIEW_INPUT {
        return None;
    }
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let preview = preview_json(&value, DEFAULT_JSON_ITEMS, 24, 5, 0);
    let preview_text = truncate_chars(
        &serde_json::to_string_pretty(&preview).unwrap_or_default(),
        MAX_SUMMARY_CHARS,
    );
    let kind = match &value {
        serde_json::Value::Array(a) => format!("array with {} item(s)", a.len()),
        serde_json::Value::Object(o) => format!("object with {} top-level key(s)", o.len()),
        _ => value.as_str().map(|_| "string".into()).unwrap_or_else(|| "value".into()),
    };
    Some(format!(
        "JSON {kind} compressed by Headroom local structural preview.\nPreview:\n{preview_text}"
    ))
}

fn summarize_plain_text(text: &str) -> Option<String> {
    if !text.chars().any(|c| !c.is_whitespace()) {
        return None;
    }
    let keep = keep_lines();
    let lines: Vec<&str> = text.split('\n').collect();
    let line_count = lines.len();

    if line_count > keep + 4 {
        let head_count = keep.div_ceil(2);
        let tail_count = keep / 2;
        let head = lines[..head_count.min(line_count)].join("\n");
        let tail_start = line_count.saturating_sub(tail_count);
        let tail = lines[tail_start..].join("\n");
        let omitted = line_count.saturating_sub(head_count + tail_count);
        return Some(format!(
            "Text output compressed by Headroom local line preview ({line_count} lines, {} chars).\n\
             First {head_count} lines:\n{head}\n\n\
             [... {omitted} lines omitted; retrieve hash for exact content ...]\n\n\
             Last {tail_count} lines:\n{tail}",
            text.len()
        ));
    }

    let excerpt = (MAX_SUMMARY_CHARS / 2).max(1_000);
    if text.len() <= excerpt * 2 + 200 {
        return None;
    }
    Some(format!(
        "Text output compressed by Headroom local character preview ({} chars).\n\
         First {excerpt} chars:\n{}\n\n\
         [... {} chars omitted; retrieve hash for exact content ...]\n\n\
         Last {excerpt} chars:\n{}",
        text.len(),
        &text[..excerpt],
        text.len().saturating_sub(excerpt * 2),
        &text[text.len() - excerpt..],
    ))
}

fn preview_json(
    value: &serde_json::Value,
    max_array: usize,
    max_keys: usize,
    max_depth: usize,
    depth: usize,
) -> serde_json::Value {
    if depth >= max_depth {
        return summarize_value(value);
    }
    match value {
        serde_json::Value::Array(arr) => {
            if arr.len() <= max_array {
                return serde_json::Value::Array(
                    arr.iter()
                        .map(|v| preview_json(v, max_array, max_keys, max_depth, depth + 1))
                        .collect(),
                );
            }
            let mut head: Vec<serde_json::Value> = arr
                .iter()
                .take(max_array)
                .map(|v| preview_json(v, max_array, max_keys, max_depth, depth + 1))
                .collect();
            head.push(serde_json::json!({
                "__headroom_omitted_items": arr.len() - max_array,
                "__headroom_total_items": arr.len(),
            }));
            serde_json::Value::Array(head)
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map.iter().take(max_keys) {
                out.insert(
                    k.clone(),
                    preview_json(v, max_array, max_keys, max_depth, depth + 1),
                );
            }
            if map.len() > max_keys {
                let omitted: Vec<String> = map.keys().skip(max_keys).cloned().collect();
                out.insert(
                    "__headroom_omitted_keys".into(),
                    serde_json::Value::Array(
                        omitted.into_iter().map(serde_json::Value::String).collect(),
                    ),
                );
            }
            serde_json::Value::Object(out)
        }
        other => other.clone(),
    }
}

fn summarize_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(a) => serde_json::json!(format!("[array:{}]", a.len())),
        serde_json::Value::Object(o) => serde_json::json!(format!("{{object:{}}}", o.len())),
        serde_json::Value::String(s) if s.len() > 200 => {
            serde_json::Value::String(format!("{}...", &s[..200]))
        }
        other => other.clone(),
    }
}

fn remember(entry: StoredContent) -> bool {
    with_store(|s| {
        let max_entries = max_store_entries();
        let max_chars = max_store_chars();
        if entry.original_chars > max_chars {
            return false;
        }
        if let Some(old) = s.entries.remove(&entry.hash) {
            s.total_chars = s.total_chars.saturating_sub(old.original_chars);
            s.order.retain(|h| h != &entry.hash);
        }
        while s.entries.len() >= max_entries
            || s.total_chars.saturating_add(entry.original_chars) > max_chars
        {
            let Some(oldest) = s.order.first().cloned() else {
                break;
            };
            if let Some(old) = s.entries.remove(&oldest) {
                s.total_chars = s.total_chars.saturating_sub(old.original_chars);
            }
            s.order.remove(0);
        }
        s.total_chars = s.total_chars.saturating_add(entry.original_chars);
        s.order.push(entry.hash.clone());
        s.entries.insert(entry.hash.clone(), entry);
        true
    })
}

fn is_protected_content(text: &str) -> bool {
    text.starts_with("<headroom_compressed")
        || text.starts_with("HEADROOM_ORIGINAL ")
        || text.starts_with("<headroom_original")
        || text.contains(&format!("`{HEADROOM_RETRIEVE_TOOL_NAME}`"))
            && text.contains("hash=\"")
            && text.len() < DEFAULT_MIN_CHARS * 2
}

fn hash_content(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn normalize_hash(hash: &str) -> Option<String> {
    let h = hash.trim().to_ascii_lowercase();
    if h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(h)
    } else {
        None
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "enable" | "enabled"
            )
        })
        .unwrap_or(false)
}

fn positive_usize_env(name: &str, default: usize, cap: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .map(|n| n.min(cap))
        .unwrap_or(default)
}

fn max_store_entries() -> usize {
    positive_usize_env(ENV_MAX_STORE_ENTRIES, DEFAULT_MAX_STORE_ENTRIES, 4096)
}

fn max_store_chars() -> usize {
    positive_usize_env(ENV_MAX_STORE_CHARS, DEFAULT_MAX_STORE_CHARS, 128 * 1024 * 1024)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// Format a human-readable stats block for `/headroom stats`.
pub fn format_stats_report() -> String {
    let st = session_stats();
    let (entries, max_e, chars, max_c) = store_stats();
    let state = if is_enabled() { "enabled" } else { "disabled" };
    format!(
        "Headroom {state} (built-in local compression)\n\
         Segments: {}/{} compressed, {} failed\n\
         Tokens: {} -> {} (saved {})\n\
         Store: {entries}/{max_e} originals, {chars}/{max_c} chars{}",
        st.compressed_segments,
        st.attempted_segments,
        st.failed_segments,
        st.tokens_before,
        st.tokens_after,
        st.tokens_saved,
        st.last_error
            .as_ref()
            .map(|e| format!("\nLast error: {e}"))
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-global Headroom state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn enable() {
        reset_for_test();
        set_enabled(true);
        set_min_chars_override(50);
        set_keep_lines_override(6);
    }

    #[test]
    fn compresses_large_text_and_retrieves() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = (0..200)
            .map(|i| format!("line-{i:04}-ABCDEFGHIJKLMNOPQRSTUVWXYZ"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(big.len() > 50, "fixture too small: {}", big.len());
        let mut stats = CompressionStats::default();
        let compressed = maybe_compress_content(&big, Some("call-1"), &mut stats)
            .unwrap_or_else(|| {
                panic!(
                    "should compress: enabled={} len={} min={}",
                    is_enabled(),
                    big.len(),
                    min_chars()
                )
            });
        assert!(stats.tokens_saved > 0);
        assert!(compressed.contains("<headroom_compressed"));
        let hash = compressed
            .split("hash=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("hash");
        let entry = retrieve(hash).expect("stored");
        assert_eq!(entry.content, big);
    }

    #[test]
    fn skips_when_disabled() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        set_enabled(false);
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content(&"x".repeat(5000), None, &mut stats).is_none());
    }

    #[test]
    fn does_not_recompress_markers() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = (0..100)
            .map(|i| format!("row{i} {}", "z".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut stats = CompressionStats::default();
        let once = maybe_compress_content(&big, Some("c1"), &mut stats)
            .expect("first compress");
        let mut stats2 = CompressionStats::default();
        assert!(maybe_compress_content(&once, Some("c1"), &mut stats2).is_none());
    }
}
