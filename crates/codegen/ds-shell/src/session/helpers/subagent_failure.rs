//! Human-readable subagent failure diagnostics.
//!
//! The subagent coordinator may report `success: false` without an error
//! string (a dropped session, an empty terminal response, a cancellation that
//! raced the completion write). Emitting the literal `"unknown error"` in
//! that case hides the only context the implementer could act on, so every
//! harness subagent spawner used to inline its own fallback. That duplicated
//! pattern is consolidated here into one context-rich message.

/// Maximum message length (keeps the gap summary bounded even when a huge
/// transcript tail would otherwise be quoted).
const MAX_MESSAGE_CHARS: usize = 500;

/// Compose a failure message for a subagent result with `success == false`.
///
/// Prefers the coordinator's own error string when present. Otherwise derives
/// a concrete description from the remaining result fields so the surfaced
/// gap is actionable even when the coordinator had nothing to say:
/// cancellation state, empty vs. non-empty output, and a bounded tail of the
/// output itself (the tail usually contains the last error the child saw).
pub(crate) fn describe_subagent_failure(
    cancelled: bool,
    error: Option<&str>,
    output: &str,
    subagent_id: &str,
    role: &str,
) -> String {
    if let Some(error) = error.map(str::trim).filter(|e| !e.is_empty()) {
        return truncate(error.to_string(), MAX_MESSAGE_CHARS);
    }

    let who = if subagent_id.trim().is_empty() {
        format!("the {role} subagent")
    } else {
        format!("the {role} subagent `{subagent_id}`")
    };
    let body = if cancelled {
        "was cancelled before it produced a verdict".to_string()
    } else if output.trim().is_empty() {
        "terminated without any output or error message".to_string()
    } else {
        let tail = output_tail(output, 200);
        format!(
            "reported failure without an error message; final output tail: {tail}"
        )
    };
    truncate(format!("{who} {body}"), MAX_MESSAGE_CHARS)
}

/// Last `max_chars` characters of the output, with a leading ellipsis marker
/// when anything was cut. Sanitized: control characters are replaced so the
/// message can be embedded in user-facing summaries.
fn output_tail(output: &str, max_chars: usize) -> String {
    let cleaned: String = output
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let tail: String = cleaned.chars().rev().take(max_chars).collect::<Vec<_>>()
        .into_iter().rev().collect();
    if cleaned.chars().count() > max_chars {
        format!("…{tail}")
    } else {
        tail
    }
}

fn truncate(mut text: String, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        text = format!("{cut}…");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_coordinator_error() {
        let message = describe_subagent_failure(
            false,
            Some("worktree creation failed"),
            "some output",
            "critic-0",
            "verification skeptic",
        );
        assert_eq!(message, "worktree creation failed");
    }

    #[test]
    fn empty_error_and_empty_output_is_actionable() {
        let message = describe_subagent_failure(false, None, "", "critic-0", "verification skeptic");
        assert_eq!(
            message,
            "the verification skeptic subagent `critic-0` terminated without any output or error message"
        );
    }

    #[test]
    fn cancellation_without_error_is_distinct() {
        let message = describe_subagent_failure(true, None, "", "", "goal planner");
        assert!(
            message.contains("cancelled before it produced a verdict"),
            "got: {message}"
        );
    }

    #[test]
    fn non_empty_output_quotes_a_bounded_tail() {
        let output = format!("prefix\n{}", "x".repeat(5000));
        let message = describe_subagent_failure(false, None, &output, "s-1", "strategist");
        assert!(message.contains("final output tail"), "got: {message}");
        assert!(message.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(message.contains("xxxxx"), "tail must surface output content");
    }

    #[test]
    fn messages_never_exceed_the_cap() {
        let huge = "y".repeat(10_000);
        let with_error = describe_subagent_failure(false, Some(&huge), "", "", "summarizer");
        assert!(with_error.chars().count() <= MAX_MESSAGE_CHARS);
    }
}
