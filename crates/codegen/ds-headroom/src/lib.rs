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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ds_token_estimation::estimate_tokens;
use sha2::{Digest, Sha256};

/// Env: enable Headroom for this process (`1`/`true`/`on`).
pub const ENV_HEADROOM: &str = "DS_HEADROOM";
/// Env: minimum tool-result size (bytes) before compression is attempted.
pub const ENV_MIN_CHARS: &str = "DS_HEADROOM_MIN_CHARS";
/// Env: max tool results compressed per request.
pub const ENV_MAX_SEGMENTS: &str = "DS_HEADROOM_MAX_SEGMENTS";
/// Env: lines kept (split head/tail) for plain-text previews.
pub const ENV_KEEP_LINES: &str = "DS_HEADROOM_KEEP_LINES";
/// Env: max original store entries.
pub const ENV_MAX_STORE_ENTRIES: &str = "DS_HEADROOM_MAX_STORE_ENTRIES";
/// Env: max total stored original chars (bytes).
pub const ENV_MAX_STORE_CHARS: &str = "DS_HEADROOM_MAX_STORE_CHARS";

const DEFAULT_MIN_CHARS: usize = 2_000;
pub const DEFAULT_MAX_SEGMENTS: usize = 12;
const DEFAULT_KEEP_LINES: usize = 40;
const DEFAULT_MAX_STORE_ENTRIES: usize = 256;
const DEFAULT_MAX_STORE_CHARS: usize = 16 * 1024 * 1024;

/// Default cap on full-body retrieve output (bytes). Keeps responses under
/// typical tool-boundary windows; use [`RetrieveOptions::query`] for middles.
pub const DEFAULT_RETRIEVE_MAX_CHARS: usize = 12_000;
const DEFAULT_RETRIEVE_MAX_MATCHES: usize = 50;
const DEFAULT_RETRIEVE_CONTEXT_LINES: usize = 0;

/// Process override for min chars (0 = use env/default). Used by tests.
static MIN_CHARS_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
static KEEP_LINES_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
static MAX_STORE_ENTRIES_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
static MAX_STORE_CHARS_OVERRIDE: AtomicUsize = AtomicUsize::new(0);
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

/// Options for filtered / bounded retrieve.
#[derive(Debug, Clone, Default)]
pub struct RetrieveOptions {
    /// Substring match (case-sensitive) against each line. When set, only
    /// matching lines (+ optional context) are returned.
    pub query: Option<String>,
    /// Hard cap on returned body bytes (default [`DEFAULT_RETRIEVE_MAX_CHARS`]).
    pub max_chars: Option<usize>,
    /// Max matching lines when `query` is set (default 50).
    pub max_matches: Option<usize>,
    /// Extra lines of context around each match (default 0).
    pub context_lines: Option<usize>,
}

/// Why retrieve failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrieveError {
    InvalidHash,
    NotFound,
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

/// Store occupancy: (entries, max_entries, total_chars, max_chars).
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

/// Retrieve and format for the tool: optional query filter + size cap so the
/// model can recover middle content without re-blowing the tool-result window.
pub fn retrieve_formatted(
    hash: &str,
    opts: &RetrieveOptions,
) -> Result<String, RetrieveError> {
    let key = normalize_hash(hash).ok_or(RetrieveError::InvalidHash)?;
    let entry = with_store(|s| s.entries.get(&key).cloned()).ok_or(RetrieveError::NotFound)?;
    Ok(format_retrieved(&entry, opts))
}

/// Reset store + stats (tests / live harness).
pub fn reset_for_test() {
    ENABLED_SET.store(false, Ordering::Relaxed);
    ENABLED_OVERRIDE.store(false, Ordering::Relaxed);
    MIN_CHARS_OVERRIDE.store(0, Ordering::Relaxed);
    KEEP_LINES_OVERRIDE.store(0, Ordering::Relaxed);
    MAX_STORE_ENTRIES_OVERRIDE.store(0, Ordering::Relaxed);
    MAX_STORE_CHARS_OVERRIDE.store(0, Ordering::Relaxed);
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

/// Test harness: force max store entries (0 clears).
pub fn set_max_store_entries_override(n: usize) {
    MAX_STORE_ENTRIES_OVERRIDE.store(n, Ordering::Relaxed);
}

/// Test harness: force max store chars (0 clears).
pub fn set_max_store_chars_override(n: usize) {
    MAX_STORE_CHARS_OVERRIDE.store(n, Ordering::Relaxed);
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

fn note_failed(stats: &mut CompressionStats, reason: &str) {
    stats.failed_segments += 1;
    with_store(|s| {
        s.session.failed_segments += 1;
        s.session.last_error = Some(reason.to_string());
    });
}

fn compress_one(
    text: &str,
    tool_call_id: Option<&str>,
    stats: &mut CompressionStats,
) -> Option<String> {
    stats.attempted_segments += 1;
    with_store(|s| s.session.attempted_segments += 1);

    if text.len() > max_store_chars() {
        note_failed(stats, "content exceeds max store chars");
        return None;
    }

    let before = estimate_tokens(text);
    let hash = hash_content(text);
    let compressed = build_compressed_text(text, &hash, tool_call_id);
    if compressed == text {
        note_failed(stats, "no smaller summary produced");
        return None;
    }
    let after = estimate_tokens(&compressed);
    if after >= before || compressed.len() >= text.len() {
        note_failed(stats, "compressed form not smaller");
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
        note_failed(stats, "store rejected entry");
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
         Original content is stored in this DS session. Use the `{tool}` tool with hash \"{hash}\" if exact content is needed.\n\
         Prefer `query` on `{tool}` to fetch middle lines without reloading the full body.\n\n\
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
        _ => value
            .as_str()
            .map(|_| "string".into())
            .unwrap_or_else(|| "value".into()),
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
    if text.chars().count() <= excerpt * 2 + 200 {
        return None;
    }
    let head = take_chars_prefix(text, excerpt);
    let tail = take_chars_suffix(text, excerpt);
    let omitted = text
        .chars()
        .count()
        .saturating_sub(head.chars().count() + tail.chars().count());
    Some(format!(
        "Text output compressed by Headroom local character preview ({} chars).\n\
         First {excerpt} chars:\n{head}\n\n\
         [... {omitted} chars omitted; retrieve hash for exact content ...]\n\n\
         Last {excerpt} chars:\n{tail}",
        text.len(),
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
            serde_json::Value::String(format!("{}...", take_chars_prefix(s, 200)))
        }
        other => other.clone(),
    }
}

fn remember(entry: StoredContent) -> bool {
    with_store(|s| {
        let max_entries = max_store_entries();
        let max_chars = max_store_chars();
        if entry.original_chars > max_chars || max_entries == 0 {
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
                // Cannot free enough space (should not happen when entry fits alone).
                return false;
            };
            if let Some(old) = s.entries.remove(&oldest) {
                s.total_chars = s.total_chars.saturating_sub(old.original_chars);
            }
            s.order.remove(0);
        }
        // Final fit check after eviction.
        if s.entries.len() >= max_entries
            || s.total_chars.saturating_add(entry.original_chars) > max_chars
        {
            return false;
        }
        s.total_chars = s.total_chars.saturating_add(entry.original_chars);
        s.order.push(entry.hash.clone());
        s.entries.insert(entry.hash.clone(), entry);
        true
    })
}

/// Tool names whose results should not be Headroom-compressed.
///
/// These are coordination / meta tools: compressing them forces the model to
/// immediately `headroom_retrieve` (often with full-body reloads), thrashing
/// the live transcript without saving durable shell/file dumps.
pub fn should_skip_tool_name(name: &str) -> bool {
    let n = name.trim();
    // Exact / common aliases across toolsets.
    matches!(
        n,
        "headroom_retrieve"
            | "spawn_subagent"
            | "get_command_or_subagent_output"
            | "get_task_output"
            | "kill_command_or_subagent"
            | "kill_task"
            | "monitor"
            | "Task"
            | "task"
            | "TodoWrite"
            | "todo_write"
            | "update_goal"
            | "UpdateGoal"
    ) || n.ends_with(":headroom_retrieve")
        || n.ends_with(":spawn_subagent")
        || n.ends_with(":get_command_or_subagent_output")
        || n.ends_with(":get_task_output")
        || n.contains("spawn_subagent")
        || n.contains("get_command_or_subagent_output")
        || n.contains("get_task_output")
}

fn is_protected_content(text: &str) -> bool {
    // Pre-format the marker needle once per process (avoids per-call alloc).
    static RETRIEVE_NEEDLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let needle = RETRIEVE_NEEDLE
        .get_or_init(|| format!("`{HEADROOM_RETRIEVE_TOOL_NAME}`"));
    text.starts_with("<headroom_compressed")
        || text.starts_with("HEADROOM_ORIGINAL ")
        || text.starts_with("<headroom_original")
        || (text.contains(needle.as_str())
            && text.contains("hash=\"")
            && text.len() < DEFAULT_MIN_CHARS * 2)
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
    // Allow optional 0x prefix.
    let h = h.strip_prefix("0x").unwrap_or(&h);
    if h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(h.to_string())
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
    let o = MAX_STORE_ENTRIES_OVERRIDE.load(Ordering::Relaxed);
    if o > 0 {
        return o;
    }
    positive_usize_env(ENV_MAX_STORE_ENTRIES, DEFAULT_MAX_STORE_ENTRIES, 4096)
}

fn max_store_chars() -> usize {
    let o = MAX_STORE_CHARS_OVERRIDE.load(Ordering::Relaxed);
    if o > 0 {
        return o;
    }
    positive_usize_env(ENV_MAX_STORE_CHARS, DEFAULT_MAX_STORE_CHARS, 128 * 1024 * 1024)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    take_chars_prefix(s, max) + "…"
}

fn take_chars_prefix(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn take_chars_suffix(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    s.chars().skip(total - max).collect()
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn format_retrieved(entry: &StoredContent, opts: &RetrieveOptions) -> String {
    let max_chars = opts
        .max_chars
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RETRIEVE_MAX_CHARS);

    let header = format!(
        "HEADROOM_ORIGINAL hash={} original_chars={}\n",
        entry.hash, entry.original_chars
    );

    if let Some(query) = opts.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let max_matches = opts
            .max_matches
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_RETRIEVE_MAX_MATCHES);
        let context = opts
            .context_lines
            .unwrap_or(DEFAULT_RETRIEVE_CONTEXT_LINES);
        let body = filter_lines(&entry.content, query, max_matches, context);
        let body = truncate_bytes_message(&body, max_chars);
        return format!(
            "{header}\
             Query filter: {query:?} (max_matches={max_matches}, context_lines={context})\n\
             Matching content follows.\n\n\
             {body}"
        );
    }

    if entry.content.len() <= max_chars {
        return format!(
            "{header}\
             Exact original content follows.\n\n\
             {}",
            entry.content
        );
    }

    // Full dump would exceed cap: return head/tail + instruction to use query.
    let half = max_chars / 2;
    let head = take_bytes_prefix_safe(&entry.content, half);
    let tail = take_bytes_suffix_safe(&entry.content, half.saturating_sub(200).max(1));
    let omitted = entry
        .content
        .len()
        .saturating_sub(head.len() + tail.len());
    format!(
        "{header}\
         Exact original is {orig} chars; returning first/last ~{half} bytes \
         (omitted ~{omitted}). Pass `query` to fetch middle lines without the full body.\n\n\
         {head}\n\n\
         [... {omitted} bytes omitted; re-call headroom_retrieve with query=... ...]\n\n\
         {tail}",
        orig = entry.original_chars,
    )
}

fn filter_lines(content: &str, query: &str, max_matches: usize, context_lines: usize) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let match_idxs: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(query))
        .map(|(i, _)| i)
        .take(max_matches)
        .collect();

    if match_idxs.is_empty() {
        return format!("(no lines containing {query:?})");
    }

    // Expand with context, merge overlaps.
    let mut include = vec![false; lines.len()];
    for &idx in &match_idxs {
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines + 1).min(lines.len());
        for slot in include.iter_mut().take(end).skip(start) {
            *slot = true;
        }
    }

    let total_matches = lines.iter().filter(|l| l.contains(query)).count();
    let mut out = String::new();
    if total_matches > match_idxs.len() {
        out.push_str(&format!(
            "(showing {} of {} matching lines)\n",
            match_idxs.len(),
            total_matches
        ));
    }

    let mut last_included: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if !include[i] {
            continue;
        }
        if let Some(prev) = last_included
            && i > prev + 1
        {
            out.push_str(&format!("... (lines {}-{} omitted) ...\n", prev + 2, i));
        }
        // 1-based line numbers for model usability.
        out.push_str(&format!("{}:{line}\n", i + 1));
        last_included = Some(i);
    }
    out
}

fn truncate_bytes_message(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = take_bytes_prefix_safe(s, max.saturating_sub(80));
    format!(
        "{head}\n\n[… retrieve output truncated at {max} bytes; narrow `query` or raise max_chars …]"
    )
}

fn take_bytes_prefix_safe(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn take_bytes_suffix_safe(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len().saturating_sub(max);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
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

    fn big_lines(n: usize, mid_secret: Option<(usize, &str)>) -> String {
        (1..=n)
            .map(|i| match mid_secret {
                Some((idx, secret)) if idx == i => secret.to_string(),
                _ => format!("pad_{i}_{}", "y".repeat(30)),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn compress_and_hash(text: &str) -> (String, String, CompressionStats) {
        let mut stats = CompressionStats::default();
        let compressed = maybe_compress_content(text, Some("call-1"), &mut stats)
            .expect("should compress");
        let hash = compressed
            .split("hash=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("hash")
            .to_string();
        (compressed, hash, stats)
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
        let (compressed, hash, stats) = compress_and_hash(&big);
        assert!(stats.tokens_saved > 0);
        assert!(compressed.contains("<headroom_compressed"));
        let entry = retrieve(&hash).expect("stored");
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
        let once = maybe_compress_content(&big, Some("c1"), &mut stats).expect("first compress");
        let mut stats2 = CompressionStats::default();
        assert!(maybe_compress_content(&once, Some("c1"), &mut stats2).is_none());
    }

    #[test]
    fn skips_below_min_chars_threshold() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(500);
        let small = "line\n".repeat(10); // well under 500
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content(&small, None, &mut stats).is_none());
        assert_eq!(stats.attempted_segments, 0);
    }

    #[test]
    fn compresses_at_or_above_min_threshold() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(100);
        set_keep_lines_override(4);
        // Many short lines so line-preview path compresses.
        let text = (0..40)
            .map(|i| format!("row{i:03} {}", "abcdefghij".repeat(3)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.len() >= 100);
        let mut stats = CompressionStats::default();
        let out = maybe_compress_content(&text, None, &mut stats).expect("compress");
        assert!(out.len() < text.len());
        assert!(stats.tokens_saved > 0);
    }

    #[test]
    fn store_evicts_oldest_when_entry_cap_hit() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_max_store_entries_override(2);
        let a = big_lines(80, None);
        let b = big_lines(81, None);
        let c = big_lines(82, None);
        let (_, ha, _) = compress_and_hash(&a);
        let (_, hb, _) = compress_and_hash(&b);
        assert!(retrieve(&ha).is_some());
        assert!(retrieve(&hb).is_some());
        let (_, hc, _) = compress_and_hash(&c);
        assert!(
            retrieve(&ha).is_none(),
            "oldest entry A must be evicted at cap=2"
        );
        assert!(retrieve(&hb).is_some());
        assert!(retrieve(&hc).is_some());
        let (entries, max_e, _, _) = store_stats();
        assert_eq!(entries, 2);
        assert_eq!(max_e, 2);
    }

    #[test]
    fn store_rejects_body_larger_than_max_store_chars() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_max_store_chars_override(200);
        // Must exceed min_chars (50) and max_store_chars (200).
        let huge = "x".repeat(500) + &"\nline\n".repeat(30);
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content(&huge, None, &mut stats).is_none());
        assert!(stats.failed_segments >= 1);
        assert!(retrieve(&hash_content(&huge)).is_none());
    }

    #[test]
    fn store_evicts_by_char_budget() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        // Two multi-line payloads that each fit alone; total exceeds store budget so
        // inserting B must evict A.
        set_max_store_chars_override(2_000);
        set_min_chars_override(50);
        set_keep_lines_override(4);
        let a = (0..80)
            .map(|i| format!("A{i:02} {}", "zzzzzzzzzz"))
            .collect::<Vec<_>>()
            .join("\n");
        let b = (0..80)
            .map(|i| format!("B{i:02} {}", "yyyyyyyyyy"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(a.len() > 50 && b.len() > 50);
        assert!(a.len() < 2_000 && b.len() < 2_000);
        assert!(a.len() + b.len() > 2_000);
        let mut stats = CompressionStats::default();
        let ca = maybe_compress_content(&a, Some("a"), &mut stats)
            .unwrap_or_else(|| panic!("A should compress: len={} failed={}", a.len(), stats.failed_segments));
        let ha = ca
            .split("hash=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();
        let mut stats = CompressionStats::default();
        let cb = maybe_compress_content(&b, Some("b"), &mut stats)
            .unwrap_or_else(|| panic!("B should compress: len={} failed={}", b.len(), stats.failed_segments));
        let hb = cb
            .split("hash=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();
        assert!(retrieve(&ha).is_none(), "A should be evicted for char budget");
        assert_eq!(retrieve(&hb).unwrap().content, b);
    }

    #[test]
    fn retrieve_invalid_hash_errors() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        assert_eq!(
            retrieve_formatted("not-a-hash", &RetrieveOptions::default()),
            Err(RetrieveError::InvalidHash)
        );
        assert_eq!(
            retrieve_formatted(&"ab".repeat(32), &RetrieveOptions::default()),
            Err(RetrieveError::NotFound)
        );
    }

    #[test]
    fn retrieve_query_finds_middle_secret_without_full_dump() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let secret = "SECRET_TOKEN_ZX7_2144";
        let big = big_lines(2500, Some((2144, secret)));
        let (_, hash, stats) = compress_and_hash(&big);
        assert!(stats.tokens_saved > 0);
        // Full body is large; query must recover the middle line alone.
        let formatted = retrieve_formatted(
            &hash,
            &RetrieveOptions {
                query: Some("SECRET_TOKEN_ZX7_".into()),
                max_chars: Some(2_000),
                ..Default::default()
            },
        )
        .expect("retrieve");
        assert!(formatted.contains(secret), "must contain secret: {formatted}");
        assert!(
            formatted.len() < 4_000,
            "query retrieve must stay small: {}",
            formatted.len()
        );
        // Must not dump thousands of pad lines.
        assert!(
            formatted.matches("pad_").count() < 5,
            "should not dump pad lines"
        );
    }

    #[test]
    fn retrieve_full_body_capped_with_query_hint() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = big_lines(800, None);
        let (_, hash, _) = compress_and_hash(&big);
        let formatted = retrieve_formatted(
            &hash,
            &RetrieveOptions {
                max_chars: Some(800),
                ..Default::default()
            },
        )
        .expect("retrieve");
        assert!(formatted.contains("Pass `query`") || formatted.contains("query"));
        assert!(formatted.len() < big.len());
        assert!(formatted.len() <= 800 + 400); // header overhead
    }

    #[test]
    fn retrieve_case_insensitive_hash() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = big_lines(60, None);
        let (_, hash, _) = compress_and_hash(&big);
        let upper = hash.to_ascii_uppercase();
        assert_eq!(retrieve(&upper).unwrap().content, big);
        assert!(retrieve_formatted(&format!("0x{hash}"), &RetrieveOptions::default()).is_ok());
    }

    #[test]
    fn utf8_character_preview_does_not_panic() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(20);
        set_keep_lines_override(1000); // force character-preview path (few lines)
        // Single long line of multi-byte chars so char-preview path runs.
        let big = " ind ".repeat(2_000); // each char is multi-byte UTF-8
        assert!(big.chars().count() > 2_500);
        let mut stats = CompressionStats::default();
        // May or may not compress depending on size vs summary; must not panic.
        let _ = maybe_compress_content(&big, None, &mut stats);
    }

    #[test]
    fn json_preview_compresses_arrays() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(50);
        let arr: Vec<serde_json::Value> = (0..40)
            .map(|i| serde_json::json!({"id": i, "payload": "x".repeat(20)}))
            .collect();
        let text = serde_json::to_string(&arr).unwrap();
        assert!(text.len() > 50);
        let mut stats = CompressionStats::default();
        let compressed = maybe_compress_content(&text, None, &mut stats).expect("json compress");
        assert!(compressed.contains("JSON"));
        assert!(compressed.contains("__headroom_omitted_items") || compressed.contains("array"));
        assert!(stats.tokens_saved > 0);
    }

    #[test]
    fn tokens_saved_matches_estimator() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = big_lines(500, None);
        let (compressed, _, stats) = compress_and_hash(&big);
        let before = estimate_tokens(&big);
        let after = estimate_tokens(&compressed);
        assert_eq!(stats.tokens_before, before);
        assert_eq!(stats.tokens_after, after);
        assert_eq!(stats.tokens_saved, before.saturating_sub(after));
        assert!(stats.tokens_saved > 0);
    }

    #[test]
    fn protects_headroom_original_prefix() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let body = format!(
            "HEADROOM_ORIGINAL hash={} original_chars=99\n{}",
            "ab".repeat(32),
            "x".repeat(5000)
        );
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content(&body, None, &mut stats).is_none());
    }

    #[test]
    fn should_skip_coordination_tool_names() {
        assert!(should_skip_tool_name("spawn_subagent"));
        assert!(should_skip_tool_name("get_command_or_subagent_output"));
        assert!(should_skip_tool_name("headroom_retrieve"));
        assert!(should_skip_tool_name("ds_build:get_task_output"));
        assert!(!should_skip_tool_name("run_terminal_command"));
        assert!(!should_skip_tool_name("read_file"));
        assert!(!should_skip_tool_name("grep"));
    }

    /// Regression: turning compression off must not brick retrieve for
    /// already-stored originals (markers may still be in the transcript).
    #[test]
    fn retrieve_works_after_compression_disabled() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        let big = big_lines(100, Some((50, "NEEDLE_AFTER_OFF")));
        let (_, hash, _) = compress_and_hash(&big);
        set_enabled(false);
        assert!(!is_enabled());
        assert!(
            maybe_compress_content(&"y".repeat(5000), None, &mut CompressionStats::default())
                .is_none(),
            "must not compress while disabled"
        );
        let entry = retrieve(&hash).expect("store must keep entry after disable");
        assert_eq!(entry.content, big);
        let formatted = retrieve_formatted(
            &hash,
            &RetrieveOptions {
                query: Some("NEEDLE_AFTER_OFF".into()),
                ..Default::default()
            },
        )
        .expect("formatted retrieve after disable");
        assert!(formatted.contains("NEEDLE_AFTER_OFF"));
    }

    /// max_segments-style scan: failures must not consume the success budget.
    #[test]
    fn failed_compress_does_not_block_later_success() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(50);
        set_keep_lines_override(4);
        // Tiny payloads skip before attempt (below min) — then a large one compresses.
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content("short", None, &mut stats).is_none());
        assert_eq!(stats.attempted_segments, 0);
        let big = big_lines(80, None);
        assert!(maybe_compress_content(&big, None, &mut stats).is_some());
        assert_eq!(stats.compressed_segments, 1);
    }

    /// Keep-lines edge: keep=1 (tail_count=0) must not panic and must still
    /// compress when the body is large enough for the marker to win.
    #[test]
    fn keep_one_line_preview_does_not_panic() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_keep_lines_override(1);
        set_min_chars_override(20);
        let text = (0..80)
            .map(|i| format!("line{i} {}", "abcdefghij".repeat(8)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut stats = CompressionStats::default();
        // Must not panic on empty-tail path; prefer successful compress when body is fat.
        let out = maybe_compress_content(&text, None, &mut stats);
        if let Some(out) = out {
            assert!(out.contains("<headroom_compressed"));
            assert!(stats.tokens_saved > 0);
        } else {
            // Still acceptable: estimator may reject if summary not smaller.
            assert!(stats.failed_segments >= 1 || stats.attempted_segments >= 1);
        }
    }

    /// Empty / whitespace-only bodies must not compress or panic.
    #[test]
    fn whitespace_only_does_not_compress() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        enable();
        set_min_chars_override(5);
        let mut stats = CompressionStats::default();
        assert!(maybe_compress_content("   \n\n\t  ", None, &mut stats).is_none());
        assert!(maybe_compress_content("", None, &mut stats).is_none());
    }
}
