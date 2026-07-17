//! `headroom_retrieve` — fetch exact tool-result originals stored by Headroom.

use serde::{Deserialize, Serialize};

use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

/// Input for `headroom_retrieve`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HeadroomRetrieveInput {
    #[schemars(
        description = "The Headroom hash from a <headroom_compressed hash=\"...\"> marker."
    )]
    pub hash: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional case-sensitive substring to match against lines of the original. \
                       Use this to recover middle content (e.g. a SECRET_ token or error line) \
                       without reloading the full body into context."
    )]
    pub query: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Max bytes of body to return (default 12000). Full originals larger than this \
                       return head/tail plus a hint to pass query."
    )]
    pub max_chars: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Max matching lines when query is set (default 50).")]
    pub max_matches: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Extra context lines around each query match (default 0).")]
    pub context_lines: Option<u32>,
}

/// Retrieve exact original content compressed by Headroom in this process.
#[derive(Debug, Default)]
pub struct HeadroomRetrieveTool;

impl crate::types::tool_metadata::ToolMetadata for HeadroomRetrieveTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::DsBuild
    }

    fn description_template(&self) -> &str {
        "Retrieve exact original content that DS compressed with Headroom. \
         Use when a <headroom_compressed hash=\"...\"> marker says exact content is needed. \
         Prefer the `query` argument to fetch specific middle lines (tokens, errors) without \
         reloading the full body — full dumps are capped to avoid re-truncation."
    }
}

impl ds_tool_runtime::Tool for HeadroomRetrieveTool {
    type Args = HeadroomRetrieveInput;
    type Output = ToolOutput;

    fn id(&self) -> ds_tool_protocol::ToolId {
        ds_tool_protocol::ToolId::new(ds_headroom::HEADROOM_RETRIEVE_TOOL_NAME)
            .expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::ds_tool_runtime::ListToolsContext,
    ) -> ds_tool_types::ToolDescription {
        ds_tool_types::ToolDescription::new(
            ds_headroom::HEADROOM_RETRIEVE_TOOL_NAME,
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
        _ctx: ds_tool_runtime::ToolCallContext,
        input: HeadroomRetrieveInput,
    ) -> Result<ToolOutput, ds_tool_runtime::ToolError> {
        // Retrieve is allowed even when compression is currently off: markers may
        // still point at store entries from earlier in the process. Compression
        // alone is gated by `is_enabled` / DS_HEADROOM.
        let opts = ds_headroom::RetrieveOptions {
            query: input.query,
            max_chars: input.max_chars.map(|n| n as usize),
            max_matches: input.max_matches.map(|n| n as usize),
            context_lines: input.context_lines.map(|n| n as usize),
        };
        match ds_headroom::retrieve_formatted(&input.hash, &opts) {
            Ok(text) => Ok(ToolOutput::Text(text.into())),
            Err(ds_headroom::RetrieveError::InvalidHash) => Ok(ToolOutput::Text(
                format!(
                    "Invalid Headroom hash '{}'. Expected a 64-char hex SHA-256 from a \
                     <headroom_compressed hash=\"...\"> marker.",
                    input.hash.trim()
                )
                .into(),
            )),
            Err(ds_headroom::RetrieveError::NotFound) => {
                let hint = if ds_headroom::is_enabled() {
                    "The original may have been evicted from the store, or was never compressed."
                        .to_string()
                } else {
                    "Headroom compression is currently off (store may still hold older entries). \
                     The original may have been evicted, or was never compressed. \
                     Enable with `/headroom on` or DS_HEADROOM=1 to compress new results."
                        .to_string()
                };
                Ok(ToolOutput::Text(
                    format!(
                        "No Headroom content found for hash '{}' in this process. {hint}",
                        input.hash.trim()
                    )
                    .into(),
                ))
            }
        }
    }
}
