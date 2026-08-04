//! `/workflows` — open the orchestrated workflow progress view.
//!
//! When a goal is active, this opens the same Claude Code–style phases |
//! agents panel as the `g` toggle (`ToggleGoalDetail`). Without an active
//! goal it falls back to the tasks list so users still get a clear menu
//! entry for multi-agent orchestration.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open the workflow / goal orchestration progress view.
pub struct WorkflowsCommand;

impl SlashCommand for WorkflowsCommand {
    fn name(&self) -> &str {
        "workflows"
    }

    fn description(&self) -> &str {
        "Open orchestrated workflow progress (phases + agents)"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/workflows"
    }

    fn aliases(&self) -> &[&str] {
        &["workflow", "wf"]
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_string());
        }
        // Prefer the goal/workflow detail when present; otherwise the
        // tasks list still surfaces running subagents.
        CommandResult::Action(Action::ShowWorkflows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn run_with_session(sid: Option<&agent_client_protocol::SessionId>) -> CommandResult {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: sid,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            pager_state: PagerLocalSnapshot::default(),
        };
        WorkflowsCommand.run(&mut ctx, "")
    }

    #[test]
    fn no_session_errors() {
        match run_with_session(None) {
            CommandResult::Error(msg) => assert!(msg.contains("No active session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn with_session_dispatches_show_workflows() {
        let sid = agent_client_protocol::SessionId::from("s1".to_string());
        assert!(matches!(
            run_with_session(Some(&sid)),
            CommandResult::Action(Action::ShowWorkflows)
        ));
    }

    #[test]
    fn name_and_aliases() {
        assert_eq!(WorkflowsCommand.name(), "workflows");
        assert!(WorkflowsCommand.aliases().contains(&"workflow"));
        assert!(WorkflowsCommand.aliases().contains(&"wf"));
    }
}
