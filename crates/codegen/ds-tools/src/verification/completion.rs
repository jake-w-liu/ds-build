//! Mandatory completion gate.
//!
//! When a message claims task completion, it MUST include a structured
//! verification block with observable evidence. Narrative claims
//! ("done," "fixed," "works now") without evidence are mechanically
//! rejected — the model cannot end a turn with an unsubstantiated claim.
//!
//! Required format for completion claims:
//!
//!   CRITERION: [what does "done" mean — from task definition]
//!   OBSERVED:  [pasted tool output, terminal logs, test results]
//!
//! The OBSERVED field must contain machine-verifiable output (URLs,
//! exit codes, test counts, build output, file:line refs) — not
//! narrative prose like "it worked" or "everything passes."

use regex::Regex;
use std::sync::LazyLock;

/// Patterns that indicate a TASK-LEVEL completion claim.
///
/// Deliberately narrow: sub-step progress ("Build finished", "step done",
/// checkmarks in status tables) does NOT trigger the gate. Only phrases
/// that unambiguously claim the WHOLE task/request is complete.
static CLAIM: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Only match "done/completed/finished" when it appears as a
        // standalone sentence-start or after a section break — not buried
        // in sub-step updates like "Build finished in 8m56s."
        re(r"(?im)^\s*(Done|Completed|Finished|Fixed|Resolved|All done)\b[.!]?\s*$"),
        re(r"(?im)^\s*(✓|✅|✔)\s*(Done|Complete|Fixed|Ready|Finished)\s*$"),
        // Explicit task-completion claims
        re(r"(?i)\b(task|request|issue|everything|all steps)\s+(is|are)\s+(done|complete|finished|resolved)\b"),
        re(r"(?i)\bready (to|for) (merge|ship|review|deploy|push)\b"),
        // "That should do it", "This completes the task" style
        re(r"(?im)^\s*(That|This)\s+(should|will|completes|finishes)\s+(do\s+it|the\s+(task|fix|change))\b"),
    ]
});

/// Required verification block markers.
static CRITERION_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?im)^\s*CRITERION\s*:"));
static OBSERVED_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?im)^\s*OBSERVED\s*:"));

/// An OBSERVED field must contain at least one of these to be valid.
static OBSERVED_EVIDENCE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"https?://[^\s]+"),                        // URL
        re(r"\b\d+\s+(passed|failed|ignored)\b"),       // test counts
        re(r"(?i)(exit code|exit status):\s*\d+"),      // exit codes
        re(r"[^\s]+\.[a-zA-Z]+:\d+"),                    // file:line
        re(r"```"),                                      // code/output block
        re(r"(?i)(Finished|Compiling)\s+\S+\s+profile"), // build output
        re(r"(?i)--version:\s+"),                         // version output
        re(r"(?i)\[main\s+\w+\]\s+"),                    // git commit
        re(r"(?i)(committed|pushed|installed)\s+(to|in)\s+"),
    ]
});

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("invalid completion gate pattern")
}

/// Check a message for completion claims without mandatory evidence.
///
/// Returns `Ok(())` if:
///   - No completion claim is made, OR
///   - Claim is backed by a VERIFICATION block with CRITERION + OBSERVED
///
/// Returns `Err(reason)` if a completion claim lacks the required block
/// or the OBSERVED field contains no machine-verifiable output.
pub fn check_completion(message: &str) -> Result<(), String> {
    // 1. Detect completion claim
    let has_claim = CLAIM.iter().any(|re| re.is_match(message));
    if !has_claim {
        return Ok(());
    }

    // 2. Require CRITERION field
    if !CRITERION_RE.is_match(message) {
        return Err(format!(
            "COMPLETION GATE: message claims completion but is missing a \
             CRITERION field. Every completion claim must include:\n\
             \n  CRITERION: [what \"done\" means — from the task definition]\n  OBSERVED:  [pasted tool output, terminal logs, or test results]\n\
             \nAdd both fields with verifiable evidence and retry."
        ));
    }

    // 3. Require OBSERVED field
    if !OBSERVED_RE.is_match(message) {
        return Err(format!(
            "COMPLETION GATE: message claims completion but is missing an \
             OBSERVED field. Every completion claim must include:\n\
             \n  CRITERION: [what \"done\" means]\n  OBSERVED:  [pasted tool output, terminal logs, or test results]\n\
             \nAdd the OBSERVED field with verifiable evidence and retry."
        ));
    }

    // 4. Extract OBSERVED content and check it contains real evidence
    let observed_content = extract_observed_field(message);
    if observed_content.is_empty() {
        return Err(
            "COMPLETION GATE: OBSERVED field is empty. \
             It must contain pasted tool output, terminal logs, \
             test results, or URLs — not narrative prose.".to_string()
        );
    }

    let has_evidence = OBSERVED_EVIDENCE
        .iter()
        .any(|re| re.is_match(&observed_content));

    if !has_evidence {
        return Err(format!(
            "COMPLETION GATE: OBSERVED field contains no verifiable evidence. \
             Found: \"{}\" — this looks like narrative prose, not actual output. \
             Paste the actual tool output, terminal log, or test result.",
            truncate(&observed_content, 100)
        ));
    }

    Ok(())
}

/// Extract the text after OBSERVED: up to the next section marker or end.
fn extract_observed_field(text: &str) -> String {
    let mut lines = text.lines();
    let mut capturing = false;
    let mut content = String::new();

    while let Some(line) = lines.next() {
        if OBSERVED_RE.is_match(line) {
            capturing = true;
            // Get text after the OBSERVED: marker
            if let Some(pos) = line.to_lowercase().find("observed:") {
                let after = line[pos + 9..].trim();
                if !after.is_empty() {
                    content.push_str(after);
                    content.push('\n');
                }
            }
            continue;
        }
        if capturing {
            // Stop at next section marker or blank+marker line
            if line.trim().is_empty() {
                // Check if next line is a new section
                continue;
            }
            if CRITERION_RE.is_match(line) || line.trim().starts_with("---") {
                break;
            }
            content.push_str(line);
            content.push('\n');
        }
    }
    content.trim().to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_claim_passes() {
        assert!(check_completion("Let me look at that file.").is_ok());
    }

    #[test]
    fn test_claim_without_criterion_fails() {
        let msg = "Done. The bug is fixed.\nOBSERVED: compiled fine";
        let err = check_completion(msg).unwrap_err();
        assert!(err.contains("CRITERION"));
    }

    #[test]
    fn test_claim_without_observed_fails() {
        let msg = "Done.\nCRITERION: the tool returns real data";
        let err = check_completion(msg).unwrap_err();
        assert!(err.contains("OBSERVED"));
    }

    #[test]
    fn test_claim_with_both_fields_passes() {
        let msg = "Done.\n\
                   CRITERION: web_search returns real internet data\n\
                   OBSERVED: go1.26.5 from https://pkg.go.dev/";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_observed_without_real_evidence_fails() {
        let msg = "Done.\n\
                   CRITERION: the fix works\n\
                   OBSERVED: it works correctly now";
        let err = check_completion(msg).unwrap_err();
        assert!(err.contains("no verifiable evidence"));
    }

    #[test]
    fn test_observed_with_url_passes() {
        let msg = "Fixed.\n\
                   CRITERION: endpoint returns 200\n\
                   OBSERVED: curl returned https://api.example.com/v1 with status 200";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_observed_with_test_counts_passes() {
        let msg = "Done.\n\
                   CRITERION: all tests pass\n\
                   OBSERVED: test result: ok. 9 passed; 0 failed; 0 ignored";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_observed_with_file_line_passes() {
        let msg = "Fixed.\n\
                   CRITERION: bug in client.rs resolved\n\
                   OBSERVED: changed endpoint in client.rs:136 from /responses to /chat/completions";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_observed_with_git_commit_passes() {
        let msg = "Done.\n\
                   CRITERION: committed and pushed\n\
                   OBSERVED: [main a5bd4d3] fix: the bug";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_observed_with_build_output_passes() {
        let msg = "Done.\n\
                   CRITERION: compiles clean\n\
                   OBSERVED:\n\
                   Compiling ds-tools v0.1.24\n\
                   Finished dev profile in 13.15s";
        assert!(check_completion(msg).is_ok());
    }

    #[test]
    fn test_extract_observed_field() {
        let msg = "Done.\n\
                   CRITERION: real search\n\
                   OBSERVED: go1.26.5 from pkg.go.dev\n\
                   Some other text after.";
        let observed = extract_observed_field(msg);
        assert!(observed.contains("go1.26.5 from pkg.go.dev"));
    }

    #[test]
    fn test_extract_observed_multiline() {
        let msg = "Done.\n\
                   CRITERION: tests pass\n\
                   OBSERVED:\n\
                   line one\n\
                   line two\n\
                   \n\
                   ---VERIFICATION---";
        let observed = extract_observed_field(msg);
        assert_eq!(observed, "line one\nline two");
    }

    #[test]
    fn test_observed_empty_fails() {
        let msg = "Done.\n\
                   CRITERION: it works\n\
                   OBSERVED:";
        let err = check_completion(msg).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn test_checkmark_claim_triggers_gate() {
        let msg = "✅ Done — the fix compiles and all tests pass. Everything is working correctly.";
        let err = check_completion(msg).unwrap_err();
        assert!(err.contains("CRITERION"));
    }
}
