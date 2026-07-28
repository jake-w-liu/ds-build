//! `/logout` -- remove auth credentials and return to the login screen.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct LogoutCommand;

impl SlashCommand for LogoutCommand {
    fn name(&self) -> &str {
        "logout"
    }

    fn description(&self) -> &str {
        "Log out and return to the login screen"
    }

    fn usage(&self) -> &str {
        "/logout [chatgpt]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim().to_ascii_lowercase().as_str() {
            "" => CommandResult::Action(Action::Logout),
            "chatgpt" | "openai" => CommandResult::Action(Action::ChatgptLogout),
            _ => CommandResult::Error("Usage: /logout [chatgpt]".into()),
        }
    }
}
