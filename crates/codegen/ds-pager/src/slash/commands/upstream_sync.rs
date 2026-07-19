//! `/upstream-sync` — sync upstream changes from xai-org/grok-build into DS Build.
//!
//! Delegates to `scripts/upstream-sync.sh`. Visible only when the script exists
//! (ds-build repo root). Development-only; not meant for regular users.

use std::process::Command;

use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};

/// Trigger the upstream sync workflow.
pub struct UpstreamSyncCommand;

const SCRIPT_RELATIVE: &str = "scripts/upstream-sync.sh";

impl SlashCommand for UpstreamSyncCommand {
    fn name(&self) -> &str {
        "upstream-sync"
    }

    fn description(&self) -> &str {
        "Sync upstream changes from xai-org/grok-build"
    }

    fn usage(&self) -> &str {
        "/upstream-sync [setup|fetch|status|review|show|map-path|mark-reviewed|help]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn visible(&self, ctx: &AppCtx) -> bool {
        ctx.cwd.join(SCRIPT_RELATIVE).exists()
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let script = std::path::Path::new(SCRIPT_RELATIVE);
        if !script.exists() {
            return CommandResult::Message(format!(
                "{} not found — are you in the ds-build repo root?",
                SCRIPT_RELATIVE,
            ));
        }

        let trimmed = args.trim();
        let mut cmd = Command::new("bash");
        cmd.arg(script);
        cmd.arg(if trimmed.is_empty() { "help" } else { trimmed });

        match cmd.output() {
            Ok(output) => {
                let mut msg = String::new();
                if !output.stdout.is_empty() {
                    msg.push_str(&String::from_utf8_lossy(&output.stdout).trim());
                }
                if !output.stderr.is_empty() {
                    if !msg.is_empty() {
                        msg.push('\n');
                    }
                    msg.push_str(&String::from_utf8_lossy(&output.stderr).trim());
                }
                if msg.is_empty() {
                    CommandResult::Handled
                } else {
                    CommandResult::Message(msg)
                }
            }
            Err(e) => CommandResult::Error(format!("Failed to run upstream-sync: {e}")),
        }
    }
}
