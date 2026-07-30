//! `/login` -- log in or re-authenticate with ChatGPT.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct LoginCommand;

impl SlashCommand for LoginCommand {
    fn name(&self) -> &str {
        "login"
    }

    fn description(&self) -> &str {
        "Log in or re-authenticate with ChatGPT"
    }

    fn usage(&self) -> &str {
        "/login"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim().to_ascii_lowercase().as_str() {
            "" | "chatgpt" | "openai" => CommandResult::Action(Action::ChatgptLogin),
            _ => CommandResult::Error("Usage: /login".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    #[test]
    fn bare_login_starts_chatgpt_login() {
        let models = ModelState::default();
        let mut ctx = super::super::tests::make_ctx(&models);

        assert!(!LoginCommand.takes_args());
        assert!(matches!(
            LoginCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::ChatgptLogin)
        ));
    }
}
