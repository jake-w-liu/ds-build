//! `mpr_validate_artifact` — deterministic, structural validator for MPR-style
//! benchmark answer artifacts.
//!
//! This is the completion-gate validator referenced by the `mpr-researcher`
//! agent profile's `completionRequirement`. It is a REAL deterministic
//! validator: it parses the artifact's machine-readable item markers
//! (`%<MPR:BEGIN id=…>` / `%<MPR:END id=…>`), checks every item's required
//! solution fields, rejects placeholders/abstentions, verifies LaTeX
//! `\begin`/`\end` balance, reports the exact SHA-256, and — in strict mode —
//! binds every tool-confirmation claim to a machine-readable
//! `evidence_manifest.json` record (no claim may ride on unstructured prose).
//!
//! It deliberately does NOT do lexical answer scoring and is NOT the
//! `score_submission.py` diagnostic: it cannot be gamed toward benchmark
//! targets, only toward a structurally complete, evidence-backed artifact.
//!
//! The completion gate treats a `ToolError` return as failure, so this tool
//! returns `Err` with a compact per-item defect list whenever the artifact
//! does not pass.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

/// Client-facing tool name (also referenced by the mpr-researcher profile's
/// `completionRequirement.tool`).
pub const MPR_VALIDATE_ARTIFACT_TOOL_NAME: &str = "mpr_validate_artifact";

/// Validator version reported in the success output. Bump on any semantic
/// change so a validator-version mismatch is visible in the trace.
pub const MPR_VALIDATOR_VERSION: u32 = 1;

/// Max artifact size this validator will read (16 MiB).
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// Max item blocks parsed (guards pathological inputs).
const MAX_ITEMS: usize = 256;

/// Subsection headings every item block must contain (the profile's
/// `required_solution_fields`, mirrored here exactly).
const REQUIRED_SUBSECTIONS: &[&str] = &[
    "assumptions and conventions",
    "auditable derivation or proof",
    "final answer",
    "independent checks",
    "tools and evidence",
    "confidence",
];

/// Markers that indicate an unfinished/placeholder item.
const PLACEHOLDER_MARKERS: &[&str] = &[
    "todo",
    "tbd",
    "fixme",
    "placeholder",
    "not attempted",
    "not solved",
    "unsolved",
    "xxxx",
];

/// Keywords that signal a tool-confirmation claim in an item. If any of
/// these appears in a block while `require_evidence_manifest` is set, the
/// item must carry a matching record in `evidence_manifest.json`.
const TOOL_CLAIM_KEYWORDS: &[&str] = &[
    "sympy",
    "numpy",
    "scipy",
    "python",
    "cas",
    "mathematica",
    "maple",
    "sage",
    "numerical",
    "simulat",
    "monte carlo",
];

/// Default artifact filename searched in the workspace when the caller does
/// not pass an explicit path (dev-set convention, first match wins).
const DEFAULT_ARTIFACT_CANDIDATES: &[&str] = &[
    "mpr100_answer_sheet_development.tex",
    "mpr100_answer_sheet.tex",
];

/// Input for `mpr_validate_artifact`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MprValidateArtifactInput {
    /// Artifact path relative to the workspace (default: the
    /// `mpr100_answer_sheet_development.tex` / `mpr100_answer_sheet.tex`
    /// in the working directory, whichever exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Artifact path relative to the workspace. Defaults to \
                       mpr100_answer_sheet_development.tex / mpr100_answer_sheet.tex \
                       in the working directory."
    )]
    pub artifact_path: Option<String>,

    /// Exact item IDs that must be present and complete (e.g.
    /// `[\"M01\", \"M05\"]`). When provided, any missing ID is a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Exact item IDs that must be present and complete. When provided, \
                       any missing ID fails the artifact."
    )]
    pub expected_items: Option<Vec<String>>,

    /// Bind every tool-confirmation claim to `evidence_manifest.json` in the
    /// artifact's directory. When `true`, any block mentioning a tool
    /// (SymPy/NumPy/Python/CAS/numerical/… ) without a matching successful
    /// manifest record fails. Recommended for benchmark runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "When true, every tool-confirmation claim must have a matching record \
                       in evidence_manifest.json (next to the artifact). Default false."
    )]
    pub require_evidence_manifest: Option<bool>,
}

/// One parsed item block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MprItemBlock {
    pub id: String,
    pub body: String,
}

/// One defect found by the validator (item-scoped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MprDefect {
    pub item: Option<String>,
    pub detail: String,
}

impl MprDefect {
    fn global(detail: impl Into<String>) -> Self {
        Self {
            item: None,
            detail: detail.into(),
        }
    }
    fn item(item: &str, detail: impl Into<String>) -> Self {
        Self {
            item: Some(item.to_string()),
            detail: detail.into(),
        }
    }
}

/// Parse `%<MPR:BEGIN id=X>` … `%<MPR:END id=X>` blocks. Structural
/// malformations (unclosed blocks, interleaved ids, duplicate ids) are
/// reported as defects.
pub fn parse_mpr_blocks(text: &str) -> (Vec<MprItemBlock>, Vec<MprDefect>) {
    let mut blocks = Vec::new();
    let mut defects = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_body = String::new();
    let mut seen = std::collections::BTreeSet::new();

    for raw in text.lines() {
        let line = raw.trim();
        if let Some(id) = extract_begin_id(line) {
            if let Some(open) = current_id.take() {
                defects.push(MprDefect::item(
                    &open,
                    "unclosed block (new BEGIN before END)",
                ));
            }
            if !seen.insert(id.clone()) {
                defects.push(MprDefect::item(&id, "duplicate item id"));
            }
            current_id = Some(id);
            current_body.clear();
            continue;
        }
        if let Some(id) = extract_end_id(line) {
            match current_id.take() {
                Some(open) if open == id => {
                    blocks.push(MprItemBlock {
                        id,
                        body: current_body.clone(),
                    });
                    if blocks.len() > MAX_ITEMS {
                        defects.push(MprDefect::global(format!(
                            "more than {MAX_ITEMS} item blocks"
                        )));
                        break;
                    }
                }
                Some(open) => {
                    defects.push(MprDefect::item(
                        &open,
                        format!("END id={id} does not match open BEGIN id"),
                    ));
                }
                None => defects.push(MprDefect::global(format!(
                    "END id={id} without a matching BEGIN"
                ))),
            }
            continue;
        }
        if current_id.is_some() {
            current_body.push_str(raw);
            current_body.push('\n');
        }
    }
    if let Some(open) = current_id {
        defects.push(MprDefect::item(&open, "unclosed block (missing END)"));
    }
    (blocks, defects)
}

fn extract_begin_id(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("%<MPR:BEGIN")?;
    let id = rest
        .split_once("id=")?
        .1
        .split(|c: char| c == '>' || c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

fn extract_end_id(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("%<MPR:END")?;
    let id = rest
        .split_once("id=")?
        .1
        .split(|c: char| c == '>' || c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Lowercased subsection text between `start_marker` and the next subsection
/// heading (or end of block).
fn subsection_text(body: &str, heading: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let Some(start) = lower.find(heading) else {
        return String::new();
    };
    let rest = &lower[start + heading.len()..];
    let end = rest
        .find("\\subsection*")
        .or_else(|| rest.find("%<mpr:end"))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Validate one item block. Returns the list of defects (empty = pass).
pub fn validate_item_block(block: &MprItemBlock) -> Vec<MprDefect> {
    let mut defects = Vec::new();
    let lower = block.body.to_ascii_lowercase();

    for heading in REQUIRED_SUBSECTIONS {
        if !lower.contains(heading) {
            defects.push(MprDefect::item(
                &block.id,
                format!("missing required subsection: {heading}"),
            ));
        }
    }

    let final_text = subsection_text(&block.body, "final answer");
    if !final_text.contains("\\boxed") {
        defects.push(MprDefect::item(
            &block.id,
            "final answer section has no \\boxed deliverable",
        ));
    }
    if final_text.contains("abstain") {
        defects.push(MprDefect::item(
            &block.id,
            "item abstained (ABSTAIN in final answer) — not a validated solution",
        ));
    }

    for marker in PLACEHOLDER_MARKERS {
        if lower.contains(marker) {
            defects.push(MprDefect::item(
                &block.id,
                format!("placeholder marker present: {marker}"),
            ));
        }
    }

    // LaTeX environment balance (comment text may overcount; a mismatch is
    // still a strong signal the block is malformed).
    let begins = count_occurrences(&block.body, "\\begin{");
    let ends = count_occurrences(&block.body, "\\end{");
    if begins != ends {
        defects.push(MprDefect::item(
            &block.id,
            format!("unbalanced \\begin{{…}} ({{begins}}) vs \\end{{…}} ({{ends}})"),
        ));
    }

    defects
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

/// Whether the block text contains a tool-confirmation keyword.
fn block_claims_tool(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    TOOL_CLAIM_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Evidence manifest shape: `{"items": {"M01": {"claims": [{tool, call_id|id,
/// status?}]}}}`. Unknown extra keys are ignored so the schema stays
/// forward-compatible.
#[derive(Debug, Default, Deserialize)]
struct EvidenceManifest {
    #[serde(default)]
    items: std::collections::BTreeMap<String, ManifestItem>,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestItem {
    #[serde(default)]
    claims: Vec<ManifestClaim>,
}

#[derive(Debug, Deserialize)]
struct ManifestClaim {
    tool: String,
    #[serde(alias = "id")]
    call_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl ManifestClaim {
    fn is_successful(&self) -> bool {
        match self
            .status
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("") | Some("success") | Some("succeeded") | Some("ok")
            | Some("passed") | Some("completed") | Some("done") => true,
            Some(_) => false,
        }
    }
}

/// Validate the artifact text. Returns defects; empty = pass.
pub fn validate_artifact_text(
    text: &str,
    expected_items: &[String],
    require_evidence_manifest: bool,
    artifact_dir: &Path,
) -> Vec<MprDefect> {
    let mut defects = Vec::new();
    let (blocks, parse_defects) = parse_mpr_blocks(text);
    defects.extend(parse_defects);
    if blocks.is_empty() && defects.is_empty() {
        defects.push(MprDefect::global("no %<MPR:BEGIN id=…> item blocks found"));
    }

    for id in expected_items {
        if !blocks.iter().any(|b| &b.id == id) {
            defects.push(MprDefect::global(format!(
                "expected item {id} missing from artifact"
            )));
        }
    }

    let manifest: Option<EvidenceManifest> = if require_evidence_manifest {
        let manifest_path = artifact_dir.join("evidence_manifest.json");
        match std::fs::read_to_string(&manifest_path) {
            Ok(body) => match serde_json::from_str::<EvidenceManifest>(&body) {
                Ok(m) => Some(m),
                Err(e) => {
                    defects.push(MprDefect::global(format!(
                        "evidence_manifest.json malformed: {e}"
                    )));
                    None
                }
            },
            Err(e) => {
                defects.push(MprDefect::global(format!(
                    "evidence_manifest.json required but unreadable: {e}"
                )));
                None
            }
        }
    } else {
        None
    };

    for block in &blocks {
        defects.extend(validate_item_block(block));
        if let Some(manifest) = &manifest
            && block_claims_tool(&block.body)
        {
            let record = manifest.items.get(&block.id);
            let claims = record.map(|r| r.claims.as_slice()).unwrap_or(&[]);
            if claims.is_empty() {
                defects.push(MprDefect::item(
                    &block.id,
                    "block claims tool confirmation but has no evidence_manifest.json \
                     record (require_evidence_manifest=true)",
                ));
            } else {
                for claim in claims {
                    if claim.tool.trim().is_empty() {
                        defects.push(MprDefect::item(
                            &block.id,
                            "evidence claim missing tool name",
                        ));
                    }
                    if claim.call_id.as_deref().unwrap_or("").trim().is_empty() {
                        defects.push(MprDefect::item(
                            &block.id,
                            "evidence claim missing call_id",
                        ));
                    }
                    if !claim.is_successful() {
                        defects.push(MprDefect::item(
                            &block.id,
                            format!(
                                "evidence claim for tool '{}' has non-success status {:?}",
                                claim.tool, claim.status
                            ),
                        ));
                    }
                }
            }
        }
    }

    defects
}

/// Compute the SHA-256 hex digest of a file.
pub fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot open artifact: {e}"))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        use std::io::Read;
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("cannot read artifact: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Resolve the artifact path: explicit input, then default candidates in
/// `cwd`. Returns the path (not yet validated as a file).
fn resolve_artifact_path(cwd: &Path, requested: Option<&str>) -> Result<std::path::PathBuf, String> {
    if let Some(raw) = requested {
        let p = Path::new(raw.trim());
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        return Ok(p);
    }
    for candidate in DEFAULT_ARTIFACT_CANDIDATES {
        let p = cwd.join(candidate);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "no MPR artifact found in {} (looked for {}) — pass artifact_path explicitly",
        cwd.display(),
        DEFAULT_ARTIFACT_CANDIDATES.join(", "),
    ))
}

/// Run the full validation. `Ok` carries the pass summary; `Err` carries the
/// defect summary that the completion gate treats as a failed validation.
pub fn validate_artifact(
    cwd: &Path,
    input: &MprValidateArtifactInput,
) -> Result<String, String> {
    let artifact = resolve_artifact_path(cwd, input.artifact_path.as_deref())?;
    let metadata = std::fs::metadata(&artifact)
        .map_err(|e| format!("cannot stat artifact {}: {e}", artifact.display()))?;
    if !metadata.is_file() {
        return Err(format!("artifact {} is not a file", artifact.display()));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "artifact {} is {} bytes (limit {MAX_ARTIFACT_BYTES})",
            artifact.display(),
            metadata.len()
        ));
    }
    let body = std::fs::read_to_string(&artifact)
        .map_err(|e| format!("cannot read artifact {}: {e}", artifact.display()))?;

    let expected: Vec<String> = input
        .expected_items
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let require_manifest = input.require_evidence_manifest.unwrap_or(false);
    let artifact_dir = artifact.parent().unwrap_or(cwd).to_path_buf();

    let defects = validate_artifact_text(&body, &expected, require_manifest, &artifact_dir);
    if !defects.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        for d in defects.iter().take(20) {
            lines.push(match &d.item {
                Some(item) => format!("- item {item}: {}", d.detail),
                None => format!("- {}", d.detail),
            });
        }
        if defects.len() > 20 {
            lines.push(format!("- … and {} more defects", defects.len() - 20));
        }
        return Err(format!(
            "mpr_validate_artifact: FAIL ({} defect(s))\n{}",
            defects.len(),
            lines.join("\n")
        ));
    }

    let digest = file_sha256(&artifact)?;
    let (blocks, _) = parse_mpr_blocks(&body);
    let ids: Vec<&str> = blocks.iter().map(|b| b.id.as_str()).collect();
    Ok(format!(
        "mpr_validate_artifact: PASS\n\
         validator_version: {MPR_VALIDATOR_VERSION}\n\
         artifact: {}\n\
         sha256: {digest}\n\
         items: {} validated\n\
         ids: {}\n\
         evidence_manifest: {}",
        artifact.display(),
        ids.len(),
        ids.join(", "),
        if require_manifest { "required+ok" } else { "not required" },
    ))
}

/// Tool: `mpr_validate_artifact`.
#[derive(Debug, Default)]
pub struct MprValidateArtifactTool;

impl crate::types::tool_metadata::ToolMetadata for MprValidateArtifactTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::DsBuild
    }

    fn description_template(&self) -> &str {
        "Deterministically validate an MPR-style answer artifact (\\%<MPR:BEGIN id=...> item \
         blocks) for structural completeness: every item's ASSUMPTIONS / DERIVATION / FINAL \
         (\\boxed) / CHECKS / TOOLS-EVIDENCE / CONFIDENCE sections present, no placeholders or \
         abstentions, balanced LaTeX environments, and (with require_evidence_manifest=true) \
         every tool-confirmation claim bound to a successful evidence_manifest.json record. \
         Returns the exact artifact SHA-256 on success and a TOOL ERROR with the per-item \
         defect list on any failure."
    }
}

impl ds_tool_runtime::Tool for MprValidateArtifactTool {
    type Args = MprValidateArtifactInput;
    type Output = ToolOutput;

    fn id(&self) -> ds_tool_protocol::ToolId {
        ds_tool_protocol::ToolId::new(MPR_VALIDATE_ARTIFACT_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::ds_tool_runtime::ListToolsContext,
    ) -> ds_tool_types::ToolDescription {
        ds_tool_types::ToolDescription::new(
            MPR_VALIDATE_ARTIFACT_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::description_template(self),
        )
    }

    fn capabilities(&self) -> ds_tool_protocol::ToolCapabilities {
        ds_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(ds_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    async fn run(
        &self,
        ctx: ds_tool_runtime::ToolCallContext,
        input: MprValidateArtifactInput,
    ) -> Result<ToolOutput, ds_tool_runtime::ToolError> {
        let cwd = ctx
            .extensions
            .get::<ds_tool_runtime::Cwd>()
            .map(|c| c.0.clone())
            .unwrap_or_default();
        match validate_artifact(cwd.as_path(), &input) {
            Ok(summary) => Ok(ToolOutput::Text(summary.into())),
            Err(details) => Err(ds_tool_runtime::ToolError::invalid_arguments(details)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_ITEM: &str = "%<MPR:BEGIN id=M01>\n\
         \\subsection*{Assumptions and conventions}\nA is invertible.\n\
         \\subsection*{Auditable derivation or proof}\nBy the lemma, det B = det A s.\n\
         \\subsection*{Final answer}\n\\[\\boxed{\\det B = s\\det A}\\]\n\
         \\subsection*{Independent checks}\nCheck 1: alpha=0 reduces to det A.\n\
         \\subsection*{Tools and evidence}\nNone\n\
         \\subsection*{Confidence}\n0.9\n\
         %<MPR:END id=M01>\n";

    #[test]
    fn parses_valid_block_roundtrip() {
        let (blocks, defects) = parse_mpr_blocks(VALID_ITEM);
        assert!(defects.is_empty(), "{defects:?}");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id, "M01");
        assert!(blocks[0].body.contains("\\boxed"));
    }

    #[test]
    fn missing_subsection_reported() {
        let body = VALID_ITEM.replace(
            "\\subsection*{Independent checks}\nCheck 1: alpha=0 reduces to det A.\n",
            "",
        );
        let (blocks, _) = parse_mpr_blocks(&body);
        let defects = validate_item_block(&blocks[0]);
        assert!(
            defects
                .iter()
                .any(|d| d.detail.contains("independent checks")),
            "{defects:?}"
        );
    }

    #[test]
    fn missing_boxed_final_reported() {
        let body = VALID_ITEM.replace("\\[\\boxed{\\det B = s\\det A}\\]", "\\[\\det B = s\\det A\\]");
        let (blocks, _) = parse_mpr_blocks(&body);
        let defects = validate_item_block(&blocks[0]);
        assert!(
            defects.iter().any(|d| d.detail.contains("\\boxed")),
            "{defects:?}"
        );
    }

    #[test]
    fn abstain_fails_item() {
        let body = VALID_ITEM.replace("\\[\\boxed{\\det B = s\\det A}\\]", "\\[\\boxed{\\text{ABSTAIN}}\\]");
        let (blocks, _) = parse_mpr_blocks(&body);
        let defects = validate_item_block(&blocks[0]);
        assert!(defects.iter().any(|d| d.detail.contains("abstain")), "{defects:?}");
    }

    #[test]
    fn placeholder_marker_reported() {
        let body = VALID_ITEM.replace("None", "TODO: fill in");
        let (blocks, _) = parse_mpr_blocks(&body);
        let defects = validate_item_block(&blocks[0]);
        assert!(defects.iter().any(|d| d.detail.contains("placeholder marker")), "{defects:?}");
    }

    #[test]
    fn unbalanced_latex_environments_reported() {
        let body = VALID_ITEM.replace("\\[\\boxed{\\det B = s\\det A}\\]", "\\begin{align} x \\\\");
        let (blocks, _) = parse_mpr_blocks(&body);
        let defects = validate_item_block(&blocks[0]);
        assert!(
            defects.iter().any(|d| d.detail.contains("unbalanced")),
            "{defects:?}"
        );
    }

    #[test]
    fn unclosed_block_reported() {
        let body = "%<MPR:BEGIN id=M01>\nbody without end\n";
        let (blocks, defects) = parse_mpr_blocks(body);
        assert!(blocks.is_empty());
        assert!(defects.iter().any(|d| d.detail.contains("unclosed")), "{defects:?}");
    }

    #[test]
    fn mismatched_end_reported() {
        let body = "%<MPR:BEGIN id=M01>\nbody\n%<MPR:END id=M02>\n";
        let (_, defects) = parse_mpr_blocks(body);
        assert!(defects.iter().any(|d| d.detail.contains("does not match")), "{defects:?}");
    }

    #[test]
    fn expected_items_missing_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sheet.tex"), VALID_ITEM).unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: Some(vec!["M01".to_string(), "M05".to_string()]),
            require_evidence_manifest: None,
        };
        let err = validate_artifact(dir.path(), &input).unwrap_err();
        assert!(err.contains("expected item M05 missing"), "{err}");
    }

    #[test]
    fn valid_artifact_passes_with_sha256() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mpr100_answer_sheet_development.tex"), VALID_ITEM).unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: None,
            expected_items: Some(vec!["M01".to_string()]),
            require_evidence_manifest: None,
        };
        let summary = validate_artifact(dir.path(), &input).unwrap();
        assert!(summary.contains("PASS"), "{summary}");
        assert!(summary.contains("sha256: "), "{summary}");
        assert!(summary.contains("validator_version: 1"), "{summary}");
    }

    #[test]
    fn no_default_artifact_errors() {
        let dir = tempfile::tempdir().unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: None,
            expected_items: None,
            require_evidence_manifest: None,
        };
        assert!(validate_artifact(dir.path(), &input).is_err());
    }

    #[test]
    fn tool_claim_without_manifest_record_fails_strict() {
        let dir = tempfile::tempdir().unwrap();
        let body = VALID_ITEM.replace(
            "\\subsection*{Tools and evidence}\nNone",
            "\\subsection*{Tools and evidence}\nSymPy confirmed the determinant numerically.",
        );
        std::fs::write(dir.path().join("sheet.tex"), &body).unwrap();
        // Manifest file missing entirely: the artifact fails with the
        // unreadable-manifest defect (the per-block claim check is skipped
        // because there is no manifest to bind against).
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: None,
            require_evidence_manifest: Some(true),
        };
        let err = validate_artifact(dir.path(), &input).unwrap_err();
        assert!(
            err.contains("evidence_manifest.json required but unreadable"),
            "{err}"
        );
    }

    #[test]
    fn tool_claim_with_manifest_but_missing_item_record_fails_strict() {
        let dir = tempfile::tempdir().unwrap();
        let body = VALID_ITEM.replace(
            "\\subsection*{Tools and evidence}\nNone",
            "\\subsection*{Tools and evidence}\nSymPy confirmed the determinant numerically.",
        );
        std::fs::write(dir.path().join("sheet.tex"), &body).unwrap();
        // Manifest exists but has no record for the claiming item.
        std::fs::write(
            dir.path().join("evidence_manifest.json"),
            r#"{"items":{"M02":{"claims":[{"tool":"sympy","call_id":"trace-other","status":"success"}]}}}"#,
        )
        .unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: None,
            require_evidence_manifest: Some(true),
        };
        let err = validate_artifact(dir.path(), &input).unwrap_err();
        assert!(err.contains("claims tool confirmation"), "{err}");
    }

    #[test]
    fn tool_claim_with_successful_manifest_passes_strict() {
        let dir = tempfile::tempdir().unwrap();
        let body = VALID_ITEM.replace(
            "\\subsection*{Tools and evidence}\nNone",
            "\\subsection*{Tools and evidence}\nSymPy confirmed the determinant numerically.",
        );
        std::fs::write(dir.path().join("sheet.tex"), &body).unwrap();
        std::fs::write(
            dir.path().join("evidence_manifest.json"),
            r#"{"items":{"M01":{"claims":[{"tool":"sympy","call_id":"trace-abc","status":"success"}]}}}"#,
        )
        .unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: None,
            require_evidence_manifest: Some(true),
        };
        let summary = validate_artifact(dir.path(), &input).unwrap();
        assert!(summary.contains("evidence_manifest: required+ok"), "{summary}");
    }

    #[test]
    fn failed_manifest_status_fails_strict() {
        let dir = tempfile::tempdir().unwrap();
        let body = VALID_ITEM.replace(
            "\\subsection*{Tools and evidence}\nNone",
            "\\subsection*{Tools and evidence}\nSymPy confirmed the determinant numerically.",
        );
        std::fs::write(dir.path().join("sheet.tex"), &body).unwrap();
        std::fs::write(
            dir.path().join("evidence_manifest.json"),
            r#"{"items":{"M01":{"claims":[{"tool":"sympy","call_id":"trace-abc","status":"error"}]}}}"#,
        )
        .unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: None,
            require_evidence_manifest: Some(true),
        };
        let err = validate_artifact(dir.path(), &input).unwrap_err();
        assert!(err.contains("non-success status"), "{err}");
    }

    #[test]
    fn tool_claim_ignored_when_manifest_not_required() {
        let dir = tempfile::tempdir().unwrap();
        let body = VALID_ITEM.replace(
            "\\subsection*{Tools and evidence}\nNone",
            "\\subsection*{Tools and evidence}\nSymPy confirmed the determinant numerically.",
        );
        std::fs::write(dir.path().join("sheet.tex"), &body).unwrap();
        let input = MprValidateArtifactInput {
            artifact_path: Some("sheet.tex".to_string()),
            expected_items: None,
            require_evidence_manifest: Some(false),
        };
        assert!(validate_artifact(dir.path(), &input).is_ok());
    }

    /// Opt-in end-to-end check against the shipped verified reference sheet.
    /// Runs only when `MPR_REFERENCE_SHEET` points at an MPR artifact (e.g.
    /// `MPR_REFERENCE_SHEET=evals/mpr100/mpr100_answer_sheet_verified.tex`):
    /// the verified reference MUST pass the validator.
    #[test]
    fn verified_reference_sheet_passes_when_available() {
        let Ok(path) = std::env::var("MPR_REFERENCE_SHEET") else {
            eprintln!("skipping: MPR_REFERENCE_SHEET not set");
            return;
        };
        // cargo test runs with cwd = the crate root; also try the repo root
        // (CARGO_MANIFEST_DIR/../../..) so a repo-relative path works either way.
        let mut candidates = vec![std::path::PathBuf::from(&path)];
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            candidates.push(
                std::path::PathBuf::from(manifest)
                    .join("../../..")
                    .join(&path),
            );
        }
        let path = candidates
            .into_iter()
            .find(|p| p.is_file())
            .expect("reference sheet not found (checked cwd and repo root)");
        assert!(path.is_file(), "reference sheet not found: {}", path.display());
        let dir = path.parent().expect("reference sheet has a parent dir");
        let input = MprValidateArtifactInput {
            artifact_path: Some(path.file_name().unwrap().to_string_lossy().to_string()),
            expected_items: None,
            require_evidence_manifest: None,
        };
        let summary = validate_artifact(dir, &input).unwrap_or_else(|e| panic!("reference sheet must pass the validator: {e}"));
        assert!(summary.contains("items: 20 validated"), "{summary}");
    }
}
