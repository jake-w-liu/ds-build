// Lint surface is intentional: unused/dead code must not be silenced at the
// crate root. Prefer local `#[allow(...)]` only with a reason comment.
pub(crate) use ds_telemetry::unified_log;
pub use ds_tracing_macros::{teprintln, timed, tprintln};
pub mod active_sessions;
pub mod agent;
pub mod auth;
pub mod chatgpt;
pub mod builtin;
pub mod bundle;
pub mod claude_import;
pub mod claude_import_state;
pub mod cli_models;
pub mod config;
pub use ds_shell_base::cpu_profile;
pub use ds_shell_base::env;
pub mod extensions;
pub use ds_workspace::foreign_sessions;
pub mod heap_profile;
pub use ds_http as http;
pub mod inspect;
pub mod instrumentation;
pub mod leader;
pub mod managed_config;
pub mod mcp_doctor;
pub use ds_models as models;
pub mod plugin;
pub mod relay;
pub mod remote;
pub mod sampling;
pub mod session;
pub mod terminal;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tier;
pub mod tools;
pub mod trace_classifier;
pub mod upload;
pub mod util;
