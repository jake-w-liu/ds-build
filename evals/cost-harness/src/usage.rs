//! Per-request usage rows + parsing of the real `ds` headless output JSON.
//!
//! The recorder (mock in mock mode, forward-proxy in live mode) emits one
//! `usage.jsonl` row per model request; every number on a row is either the
//! real gateway usage object (live) or the mock's body-derived estimate
//! (mock), and cost is recomputable from the raw rows × [`crate::cost`].

use serde::{Deserialize, Serialize};

/// One recorded model request's token accounting (gateway-billing shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRow {
    /// Monotonic request sequence across the whole run (wire.jsonl seq).
    pub seq: u64,
    /// `x-ds-conv-id` header (session id; empty for title/aux requests).
    pub conv_id: String,
    /// `x-ds-req-id` header (uuid, or `ds-compact-*` / `ds-recap-*`).
    pub req_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

impl UsageRow {
    /// Cache-hit fraction of the prompt, 0..=1 (0 when input is 0).
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        (self.cache_read_tokens as f64 / self.input_tokens as f64).clamp(0.0, 1.0)
    }

    pub fn is_compaction(&self) -> bool {
        self.req_id.starts_with("ds-compact-")
    }

    pub fn is_recap(&self) -> bool {
        self.req_id.starts_with("ds-recap-")
    }

    /// Main-conversation request (not title/aux/compaction/recap/subagent).
    pub fn is_main(&self) -> bool {
        !self.req_id.starts_with("ds-compact-")
            && !self.req_id.starts_with("ds-recap-")
            && !self.req_id.starts_with("ds-")
            && !self.conv_id.is_empty()
    }
}

/// The usage object `ds -p … --output-format json` returns on success.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DsUsage {
    pub input_tokens: u64,
    #[serde(default, rename = "cache_read_input_tokens")]
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// The headless output JSON: success has `text`; failure has `type == "error"`.
#[derive(Debug, Clone, Deserialize)]
pub struct DsOutput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub usage: Option<DsUsage>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

impl DsOutput {
    /// True when the session reported a clean end-turn (not an error).
    pub fn succeeded(&self) -> bool {
        self.text.is_some() && self.kind.as_deref() != Some("error")
    }

    /// True when the session errored (auth, config, gate, …).
    pub fn failed(&self) -> bool {
        self.kind.as_deref() == Some("error") || self.text.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_SUCCESS: &str = r#"{
        "text": "done reading.",
        "stopReason": "EndTurn",
        "sessionId": "019fc74b-2a23-7582-b9d9-abb704796eb9",
        "usage": {
            "input_tokens": 9339,
            "cache_read_input_tokens": 12416,
            "output_tokens": 968,
            "reasoning_tokens": 605,
            "total_tokens": 22723
        },
        "num_turns": 2
    }"#;

    const REAL_ERROR: &str = r#"{"type":"error","message":"Not signed in."}"#;

    #[test]
    fn parses_real_success_output() {
        let o: DsOutput = serde_json::from_str(REAL_SUCCESS).unwrap();
        assert!(o.succeeded());
        assert!(!o.failed());
        let u = o.usage.unwrap();
        assert_eq!(u.input_tokens, 9339);
        assert_eq!(u.cache_read_input_tokens, 12416);
        assert_eq!(u.output_tokens, 968);
        assert_eq!(u.reasoning_tokens, 605);
        assert_eq!(u.total_tokens, 22723);
        assert_eq!(o.num_turns, Some(2));
    }

    #[test]
    fn parses_real_error_output() {
        let o: DsOutput = serde_json::from_str(REAL_ERROR).unwrap();
        assert!(o.failed());
        assert!(!o.succeeded());
        assert_eq!(o.message.as_deref(), Some("Not signed in."));
    }

    #[test]
    fn cache_hit_ratio_and_classification() {
        let row = UsageRow {
            seq: 1,
            conv_id: "c".into(),
            req_id: "ds-compact-x".into(),
            model: "deepseek-v4-flash".into(),
            input_tokens: 10_000,
            cache_read_tokens: 9_728,
            output_tokens: 18,
            reasoning_tokens: 0,
            total_tokens: 10_018,
        };
        assert!((row.cache_hit_ratio() - 0.9728).abs() < 1e-9);
        assert!(row.is_compaction());
        assert!(!row.is_main());

        let main = UsageRow {
            seq: 2,
            conv_id: "019fc74b-2a23-7582-b9d9-abb704796eb9".into(),
            req_id: "edefb9aa-3578-41f3-b2b3-101623c296ac".into(),
            model: "deepseek-v4-flash".into(),
            input_tokens: 9_827,
            cache_read_tokens: 9_728,
            output_tokens: 18,
            reasoning_tokens: 0,
            total_tokens: 9_845,
        };
        assert!(main.is_main());
        assert!(!main.is_compaction());
    }
}
