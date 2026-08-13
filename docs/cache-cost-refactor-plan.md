# DS Build cache-cost refactor plan (2026-08-14)

## Objective

Learn from `deepseek-harness` and keep DS Build maximally efficient on
DeepSeek context-cache pricing while keeping correctness and full reasoning
quality. Reasoning is intentionally left **enabled and maximized** across the
product (default `reasoning_effort = "max"`); no call site opts it down.

## What we learned from deepseek-harness

The harness (`@deepseek-ai/dsh-llm-deepseek`) documents the DeepSeek wire
contract precisely. The transferable rules relevant here:

1. **Reasoning passback rule** — `reasoning_content` must be replayed on
   assistant turns that carried tool calls; on tool-call-free turns it is
   ignored, so it is dropped to save input tokens.
2. **`content` is never `null`** on assistant messages (empty string is the
   safe floor).
3. **Empty tool output still needs content** on the wire — `(no output)`.
4. **Cache accounting is disjoint** — `input_tokens = prompt_tokens - cache_hit`,
   because DeepSeek `prompt_tokens` already includes cache hits.

## Findings (DS Build already has most of this)

Verified against source, not assumed:

- Reasoning passback rule: implemented and tested in
  `crates/codegen/ds-sampling-types/src/conversation.rs`
  (`conversation_to_chat_messages`; tests
  `test_tool_calls_turn_carries_reasoning_content`,
  `test_non_tool_assistant_omits_reasoning_content`).
- Assistant `content` never null: `conversation_item_to_chat_message` emits
  `MessageContent::Text` (empty string allowed, never null).
- Cache accounting disjoint: `TokenUsage` keeps full `prompt_tokens` plus a
  separate `cached_prompt_tokens` subset; `ds-models` cost math bills the two
  at their own rates (`crates/codegen/ds-models/src/pricing.rs`).
- Stable prefix for KV-cache: tool definitions sorted alphabetically,
  hosted tools last, reasoning siblings preserved in interleaved order,
  project-instructions "once placed, never replaced" — enforced by the
  KV Cache Invariant Tests block in `conversation.rs`.
- Headroom compression reduces prompt size while keeping the prefix stable
  (`evals/cost-harness` measures a live ~84% saving on big tool results).

## Gap fixed

### Empty tool result could be sent as an empty string (correctness)

DeepSeek rejects `role: "tool"` with empty `content`. The chat-completions
conversion emitted `t.content` verbatim, so a tool run with no stdout/result
text would produce an empty string and a 400.

Fix: normalize empty tool-result content to `(no output)` in the
chat-completions conversion (`tool_result_content`), matching the harness.

File: `crates/codegen/ds-sampling-types/src/conversation.rs`

## Deliberately not changed (with rationale)

- **Reasoning/thinking effort**: left at the user's default (`max`) on every
  call path, including auxiliary calls. This is a hard product invariant
  (documented in [`README.md`](../README.md)) — a quality-over-cost decision
  that must not be reversed by future token-saving work.
- **Responses / Messages empty-tool-result fallback**: the harness documents
  the empty-content requirement for the chat-completions route only; the
  other backends are left unchanged until a live 400 proves they need it.
- **Attribution headers** (`x-deepseek-harness-*`): DS Build already sends
  more specific identity headers (`x_ds_conv_id`, `x_ds_req_id`,
  `x_ds_session_id`, `x_ds_turn_idx`, `x_ds_agent_id`).

## Verification

- `cargo check -p ds-sampling-types` — pass.
- `cargo test -p ds-sampling-types --lib` — pass.
- New unit test: `empty_tool_result_content_uses_no_output_fallback`.
- Final gate: `bump-and-install.sh` (clean bake, push, install, codesign).
