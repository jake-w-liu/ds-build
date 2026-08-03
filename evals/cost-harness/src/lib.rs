//! ds-cost-harness — cost-aware stress/verification harness for the DS Build
//! DeepSeek pipeline.
//!
//! Two goals, measured, not assumed:
//! 1. **Save tokens/money** — headroom ON vs OFF A/B on a big-tool-result
//!    scenario, with cost computed from real per-request usage rows ×
//!    [`ds_models`] pinned rates, and compression stats from the shipped
//!    [`ds_headroom`] functions.
//! 2. **Ensure correctness** — stress scenarios run through the real `ds`
//!    binary (headless `-p --output-format json`); wire-contract assertions
//!    (reasoning_content rules, headroom markers + byte-exact retrieve,
//!    completion gate) drive the shipped functions.
//!
//! Modes:
//! - `mock` (default, free, deterministic): a local recording mock serves
//!   `/v1/chat/completions` with scripted tool-call/text streams and usage
//!   derived from the request body it actually received. Every request is
//!   recorded to `wire.jsonl` + `usage.jsonl` — the same capture format live
//!   mode uses, so cost math is mode-independent.
//! - `live`: a recording forward-proxy relays to `api.deepseek.com` and parses
//!   the real usage object from the SSE stream. Requires `DEEPSEEK_API_KEY`
//!   (or `DS_API_KEY`, or the key in `~/.ds/config.toml`). Expected cost per
//!   full pass: see README (flash list rates, ~$0.02–0.05).

pub mod assertions;
pub mod cost;
pub mod mock;
pub mod report;
pub mod runner;
pub mod scenarios;
pub mod session;
pub mod usage;
