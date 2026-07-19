//! Output verification gate — native ds-build enforcement that a tool's
//! output actually does what it claims, not dependent on any external hook
//! infrastructure (Claude Code, etc.).
//!
//! Every tool that claims external access (web, filesystem, network) should
//! run its output through [`verify`] before returning it to the model. The
//! gate checks for disqualifying phrases that indicate the output is
//! fabricated rather than observed.

mod phrases;

pub use phrases::Disqualification;
use phrases::verify as verify_impl;

/// Run the verification gate on tool output.
///
/// Returns `Ok(())` if the output passes, or `Err(Disqualification)` with
/// the reason and the offending phrase if it fails.
///
/// # Example
///
/// ```ignore
/// let (content, citations) = client.search(query, domains).await?;
/// if let Err(dq) = verification::verify("web_search", &content, &citations) {
///     return Err(ToolError::execution(id, dq.to_string()));
/// }
/// Ok(WebSearchOutput { content, citations, ... })
/// ```
pub fn verify(
    tool_name: &str,
    output_text: &str,
    citations: &[String],
) -> Result<(), Disqualification> {
    verify_impl(tool_name, output_text, citations)
}
