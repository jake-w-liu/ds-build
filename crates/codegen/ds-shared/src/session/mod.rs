use std::path::PathBuf;

pub mod info;

pub use info::Info;

// Re-export shared feedback wire types used by downstream crates
// (e.g. ds-pager-render).
pub use ds_cli_proxy_types::feedback_types::FeedbackTerminalInfo;

pub fn session_dir(info: &Info) -> PathBuf {
    ds_tools::util::ds_home::sessions_cwd_dir(&info.cwd).join(info.id.to_string())
}
