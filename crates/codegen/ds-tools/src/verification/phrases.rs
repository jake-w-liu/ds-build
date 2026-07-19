use regex::Regex;
use std::sync::LazyLock;

/// A verification failure with the tool name, reason, and matched phrase.
#[derive(Debug, Clone)]
pub struct Disqualification {
    pub tool_name: String,
    pub reason: String,
    pub matched_phrase: String,
}

impl std::fmt::Display for Disqualification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} output FAILED verification: {} (matched: \"{}\")",
            self.tool_name, self.reason, self.matched_phrase
        )
    }
}

// ── Disqualifying phrase patterns ──────────────────────────────────────────
//
// These patterns detect tool output that was fabricated by an LLM rather than
// produced by actually exercising the tool's claimed capability.
//
// Categories:
//   CUTOFF   — model admits it has no live data
//   FABRICATE — model describes what it *would* do rather than what it did
//   EMPTY    — no real citations/URLs when tool claims external access

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Cutoff,
    Fabricate,
    Empty,
}

struct Pattern {
    re: Regex,
    category: Category,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    vec![
        // ── FABRICATE: most specific first (checked before broader cutoff patterns) ──
        p(r"as if I had searched", Category::Fabricate),
        p(r"every (search )?result is based on a knowledge cutoff", Category::Fabricate),
        p(r"no search returned a real", Category::Fabricate),
        p(r"projected estimate", Category::Fabricate),
        p(r"(simulated|hypothetical|synthetic) (search|result)", Category::Fabricate),
        p(r"I (would|could|might|can) (search|look|find|query|retrieve)", Category::Fabricate),
        p(r"(if I were|if I had|were I) (to |able to )?(search|access|look)", Category::Fabricate),
        p(r"based on (my |the |available )?(training|knowledge|internal) (data|information)", Category::Fabricate),
        p(r"(appears|seems) to be (frozen|stale|outdated|from|limited)", Category::Fabricate),

        // ── CUTOFF: model admits stale/live-data limitations ──
        p(r"knowledge cutoff", Category::Cutoff),
        p(r"as of my training", Category::Cutoff),
        p(r"training data (only goes|cutoff|is limited)", Category::Cutoff),
        p(r"I (don't|do not|cannot|can't) have (access to |the ability to )?(real-?time |live )?(data|information|internet|search)", Category::Cutoff),
        p(r"my (knowledge|information|data) (is (limited to|frozen|from)|only (goes|extends|covers))", Category::Cutoff),
        p(r"(cannot|can't|unable to) (currently )?access the (live |real-?time )?internet", Category::Cutoff),

        // ── EMPTY: tool claims external access but returns nothing ──
        p(r"no search results found", Category::Empty),
        p(r"0 results", Category::Empty),
    ]
});

fn p(pattern: &str, category: Category) -> Pattern {
    Pattern {
        re: Regex::new(&format!("(?i){}", pattern)).expect("failed to compile verification pattern"),
        category,
    }
}

/// Run all verification checks on tool output.
///
/// - `tool_name`: name of the tool (e.g., "web_search")
/// - `output_text`: the text content the tool produced
/// - `citations`: URLs or references the tool claims to have found
pub fn verify(
    tool_name: &str,
    output_text: &str,
    citations: &[String],
) -> Result<(), Disqualification> {
    check_phrases(tool_name, output_text)?;

    // Citation checks for tools that claim external data access
    let citation_tools = ["web_search", "web_fetch"];
    if citation_tools.contains(&tool_name) && citations.is_empty() {
        return Err(Disqualification {
            tool_name: tool_name.to_string(),
            reason: "tool claims external access but returned zero citations/URLs".to_string(),
            matched_phrase: "empty citations".to_string(),
        });
    }

    Ok(())
}

/// Check only the text content for disqualifying phrases (no citation
/// requirement). Used by the system-level harness.
pub fn check_phrases(
    tool_name: &str,
    output_text: &str,
) -> Result<(), Disqualification> {
    for pattern in PATTERNS.iter() {
        if let Some(m) = pattern.re.find(output_text) {
            let reason = match pattern.category {
                Category::Cutoff => {
                    "output indicates stale/live-data limitation — tool did not access real-time data"
                }
                Category::Fabricate => {
                    "output describes simulated or fabricated results — tool did not actually execute"
                }
                Category::Empty => {
                    "output contains no real results — empty or null response"
                }
            };
            return Err(Disqualification {
                tool_name: tool_name.to_string(),
                reason: reason.to_string(),
                matched_phrase: m.as_str().to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passes_valid_output() {
        assert!(verify(
            "web_search",
            "Rust 1.89.0 was released on 2025-08-07.",
            &["https://blog.rust-lang.org/2025/08/07/Rust-1.89.0.html".into()]
        )
        .is_ok());
    }

    #[test]
    fn test_rejects_cutoff_phrase() {
        let err = verify(
            "web_search",
            "Based on my knowledge cutoff, I cannot provide current data.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("stale"));
        assert!(err.matched_phrase.contains("knowledge cutoff"));
    }

    #[test]
    fn test_rejects_fabricate_phrase() {
        let err = verify(
            "web_search",
            "I would search for that if I had internet access.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("simulated"));
    }

    #[test]
    fn test_rejects_projected_estimate() {
        let err = verify(
            "web_search",
            "The search returned a projected estimate rather than the exact current version.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("simulated"));
    }

    #[test]
    fn test_rejects_as_if_searched() {
        let err = verify(
            "web_search",
            "Provide an answer as if I had searched the web.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("simulated"));
    }

    #[test]
    fn test_rejects_empty_citations_for_web_search() {
        let err = verify("web_search", "Some result text.", &[]).unwrap_err();
        assert!(err.reason.contains("zero citations"));
    }

    #[test]
    fn test_allows_empty_citations_for_non_citation_tool() {
        assert!(verify("bash", "some output", &[]).is_ok());
    }

    #[test]
    fn test_rejects_no_search_returned_real() {
        let err = verify(
            "web_search",
            "No search returned a real, current version number.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("simulated"));
    }

    #[test]
    fn test_rejects_every_result_is_based_on_cutoff() {
        let err = verify(
            "web_search",
            "Every search result is based on a knowledge cutoff.",
            &["https://example.com".into()],
        )
        .unwrap_err();
        assert!(err.reason.contains("simulated"));
    }
}
