//! Completion gate — mechanical enforcement that completion claims
//! must be backed by observable evidence.
//!
//! When the assistant claims a task is "done," "complete," "fixed," etc.,
//! this gate checks that the message also contains verifiable evidence:
//! pasted tool output, file:line citations, terminal command output,
//! or explicit verification blocks.
//!
//! A completion claim without evidence is rejected.

use regex::Regex;
use std::sync::LazyLock;

/// Patterns that indicate a completion claim.
static COMPLETION_CLAIMS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)\b(done|completed|finished|fixed|resolved|implemented)\b"),
        re(r"(?i)\b(task|issue|bug|feature) (is|has been) (done|complete|fixed|resolved|implemented)\b"),
        re(r"(?i)\b(all|everything) (is|works|working|done|complete)\b"),
        re(r"(?i)\bthis (should|will|does) (fix|resolve|address|complete)\b"),
        re(r"(?i)\bready (to|for) (merge|ship|review|deploy|push)\b"),
        re(r"(?i)\b(✓|✅|✔)\s*(done|complete|fixed|ready|finished)\b"),
    ]
});

/// Patterns that constitute verifiable evidence.
static EVIDENCE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Pasted command output (contains pid, exit code, or typical bash output)
        re(r"(?i)\b(exit code|exit status):\s*\d+"),
        re(r"(?i)\b(PID|process)\s*\d+"),
        // File:line references
        re(r"[^\s]+\.[a-zA-Z]+:\d+"),
        // Pasted terminal output (contains common CLI patterns)
        re(r"(?i)(running|tests? (passed|failed|run)|compiling|building|installing|pushing)"),
        re(r"\b\d+\s+(passed|failed|ignored|measured|filtered out)\b"),
        // Explicit verification block
        re(r"(?i)VERIFICATION"),
        re(r"(?i)DONE CRITERION"),
        re(r"(?i)OBSERVED:"),
        // Observable output from tools
        re(r"```"),
        re(r"(?i)(stdout|stderr|output):"),
        // Git operations (commit, push)
        re(r"(?i)\[main\s+\w+\]\s+"),
        re(r"(?i)(committed|pushed|merged)\s+(to|into)\s+"),
        // Install/version output
        re(r"(?i)(installed|installing)\s+(to|in)\s+"),
        re(r"(?i)--version:\s+"),
        // URL citations from real search
        re(r"https?://[^\s]+"),
        // Build output
        re(r"(?i)(Finished|Compiling)\s+\S+\s+profile"),
    ]
});

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("failed to compile completion gate pattern")
}

/// Result of checking a message for completion claims.
#[derive(Debug, Clone)]
pub struct CompletionGateResult {
    /// Whether a completion claim was detected.
    pub claims_completion: bool,
    /// Whether evidence was found alongside the claim.
    pub has_evidence: bool,
    /// The completion phrases that matched (if any).
    pub matched_claims: Vec<String>,
    /// The evidence phrases that matched (if any).
    pub matched_evidence: Vec<String>,
}

/// Check a message for completion claims without evidence.
///
/// Returns `Ok(())` if no completion claim is made, or if claims are
/// backed by evidence. Returns `Err(reason)` if a completion claim
/// is detected without accompanying evidence.
pub fn check_completion(message: &str) -> Result<CompletionGateResult, String> {
    let matched_claims: Vec<String> = COMPLETION_CLAIMS
        .iter()
        .filter_map(|re| re.find(message))
        .map(|m| m.as_str().to_string())
        .collect();

    let matched_evidence: Vec<String> = EVIDENCE_PATTERNS
        .iter()
        .filter_map(|re| re.find(message))
        .map(|m| m.as_str().to_string())
        .collect();

    let result = CompletionGateResult {
        claims_completion: !matched_claims.is_empty(),
        has_evidence: !matched_evidence.is_empty(),
        matched_claims,
        matched_evidence,
    };

    if result.claims_completion && !result.has_evidence {
        Err(format!(
            "COMPLETION GATE: message claims task completion (matched: {:?}) \
             but contains no verifiable evidence. A completion claim must be \
             accompanied by pasted tool output, terminal logs, file:line \
             citations, or an explicit VERIFICATION block with OBSERVED output.",
            result.matched_claims.first().map(|s| s.as_str()).unwrap_or("?")
        ))
    } else {
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_claim_passes() {
        let result = check_completion("Let me look at that file.");
        assert!(result.is_ok());
        let gate = result.unwrap();
        assert!(!gate.claims_completion);
    }

    #[test]
    fn test_claim_with_evidence_passes() {
        let msg = "Done. The fix compiles clean.\n\
                   ```\n\
                   Finished dev profile in 13.15s\n\
                   ```\n\
                   tests passed: 9 passed; 0 failed";
        let result = check_completion(msg);
        assert!(result.is_ok(), "Should pass: {:?}", result.err());
        let gate = result.unwrap();
        assert!(gate.claims_completion);
        assert!(gate.has_evidence);
    }

    #[test]
    fn test_claim_without_evidence_fails() {
        let msg = "Done. The bug is fixed. Everything works now.";
        let result = check_completion(msg);
        assert!(result.is_err(), "Should fail: no evidence");
        assert!(result.unwrap_err().contains("COMPLETION GATE"));
    }

    #[test]
    fn test_claim_with_file_line_passes() {
        let msg = "Fixed the bug in client.rs:136.\n\
                   The root cause was the wrong endpoint URL.";
        let result = check_completion(msg);
        assert!(result.is_ok(), "file:line should count as evidence");
    }

    #[test]
    fn test_claim_with_url_passes() {
        let msg = "Done. Results from https://releases.rs/ show the latest version.";
        let result = check_completion(msg);
        assert!(result.is_ok(), "URL should count as evidence");
    }

    #[test]
    fn test_claim_with_test_output_passes() {
        let msg = "Fixed. Tests: 10 passed; 0 failed; 0 ignored.";
        let result = check_completion(msg);
        assert!(result.is_ok(), "test output should count as evidence: {:?}", result.err());
    }

    #[test]
    fn test_claim_with_verification_block_passes() {
        let msg = "Done.\n---VERIFICATION---\nOBSERVED: compiled and tested\n---";
        let result = check_completion(msg);
        assert!(result.is_ok(), "VERIFICATION block should count as evidence");
    }

    #[test]
    fn test_claim_with_git_commit_passes() {
        let msg = "Fixed and pushed.\n[main a5bd4d3] fix: the bug";
        let result = check_completion(msg);
        assert!(result.is_ok(), "git commit should count as evidence");
    }

    #[test]
    fn test_fake_claim_fails() {
        // This is what I did — claimed "done" but only showed compile output,
        // not actual verification
        let msg = "Done. It compiles and all tests pass. \
                   The web_search tool now returns real results. \
                   Everything is working correctly.";
        // Note: "compiles" and "tests pass" are narrative claims, not evidence.
        // The gate should catch this.
        let result = check_completion(msg);
        assert!(result.is_err(), "Narrative claims without pasted output should fail");
    }
}
