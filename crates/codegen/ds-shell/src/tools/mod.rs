//! Tool infrastructure for ds-shell.
//!
//! All tool execution goes through `ds-tools` via the `ToolBridge`.
//! Types (ToolOutput, ToolInput, TodoState, etc.) come from `ds-tools` directly.

pub mod bridge;
pub mod config;
pub mod notification_bridge;
pub mod retry;
pub mod todo;
pub mod tool_context;

pub use self::{
    config::{BashToolConfig, FileToolset, ShellToolsetConfig},
    retry::{RetryConfig, execute_with_retry},
    tool_context::ToolContext,
};

// Re-export key types from ds-tools for convenience
pub use self::todo::{TodoId, TodoItem, TodoPriority, TodoStatus};
pub use ds_tools::types::output::ToolOutput;
pub use ds_tools::types::{MCPToolInput, ToolInput};
