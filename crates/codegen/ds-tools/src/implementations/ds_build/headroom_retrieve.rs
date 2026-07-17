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
         Use when a <headroom_compressed hash=\"...\"> marker says exact content is needed \
         (e.g. a middle line, full log, or full JSON body not present in the preview)."
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
        if !ds_headroom::is_enabled() {
            return Ok(ToolOutput::Text(
                "Headroom is disabled. Enable with `/headroom on` or DS_HEADROOM=1.".into(),
            ));
        }
        match ds_headroom::retrieve(&input.hash) {
            Some(entry) => {
                let text = format!(
                    "HEADROOM_ORIGINAL hash={} original_chars={}\n\
                     Exact original content follows.\n\n\
                     {}",
                    entry.hash, entry.original_chars, entry.content
                );
                Ok(ToolOutput::Text(text.into()))
            }
            None => Ok(ToolOutput::Text(
                format!(
                    "No Headroom content found for hash '{}' in this process. \
                     The original may have been evicted from the store or Headroom was off when the content was compressed.",
                    input.hash.trim()
                )
                .into(),
            )),
        }
    }
}
