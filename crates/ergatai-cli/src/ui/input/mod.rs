//! Slash-command parser (dialoguer-free).
//!
//! The old `read_user_input` / `print_help` helpers are gone — the TUI
//! handles input directly. This module now only owns the command grammar
//! shared by `runner.rs`.
//!
//! Phase 4: extended with `/model`, `/cost`, `/compact` commands.

pub mod complete;
pub mod history;

/// Parsed chat command from user input.
pub enum ChatCommand {
    /// Show help message
    Help,
    /// Exit the chat
    Quit,
    /// Clear the screen
    Clear,
    /// List available agents
    Agents,
    /// Switch to a different agent
    Switch(String),
    /// Show current status
    Status,
    /// Phase 4: show or set the model name.
    /// `None` = show current; `Some(name)` = set to `name`.
    Model(Option<String>),
    /// Phase 4: show session cost / token usage.
    Cost,
    /// Phase 4: compact conversation context (placeholder).
    Compact,
    /// Send a prompt to the agent
    SendPrompt(String),
}

/// Parse user input into a ChatCommand.
///
/// Lines starting with `/` are slash commands; everything else is a prompt.
pub fn parse_input(input: &str) -> ChatCommand {
    let trimmed = input.trim();

    if !trimmed.starts_with('/') {
        return ChatCommand::SendPrompt(trimmed.to_string());
    }

    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());

    match cmd {
        "/help" | "/h" => ChatCommand::Help,
        "/quit" | "/q" | "/exit" => ChatCommand::Quit,
        "/clear" => ChatCommand::Clear,
        "/agents" => ChatCommand::Agents,
        "/switch" | "/s" => match arg {
            Some(agent) => ChatCommand::Switch(agent.to_string()),
            None => ChatCommand::Help,
        },
        "/status" => ChatCommand::Status,
        // Phase 4: new commands.
        "/model" | "/m" => ChatCommand::Model(arg.map(|s| s.to_string())),
        "/cost" => ChatCommand::Cost,
        "/compact" => ChatCommand::Compact,
        // Unknown command — treat as prompt
        _ => ChatCommand::SendPrompt(trimmed.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_regular_message() {
        match parse_input("hello world") {
            ChatCommand::SendPrompt(text) => assert_eq!(text, "hello world"),
            _ => panic!("Expected SendPrompt"),
        }
    }

    #[test]
    fn test_parse_help() {
        assert!(matches!(parse_input("/help"), ChatCommand::Help));
        assert!(matches!(parse_input("/h"), ChatCommand::Help));
    }

    #[test]
    fn test_parse_quit() {
        assert!(matches!(parse_input("/quit"), ChatCommand::Quit));
        assert!(matches!(parse_input("/q"), ChatCommand::Quit));
        assert!(matches!(parse_input("/exit"), ChatCommand::Quit));
    }

    #[test]
    fn test_parse_switch_with_agent() {
        match parse_input("/switch claude") {
            ChatCommand::Switch(agent) => assert_eq!(agent, "claude"),
            _ => panic!("Expected Switch"),
        }
        match parse_input("/s codex") {
            ChatCommand::Switch(agent) => assert_eq!(agent, "codex"),
            _ => panic!("Expected Switch"),
        }
    }

    #[test]
    fn test_parse_switch_without_agent() {
        assert!(matches!(parse_input("/switch"), ChatCommand::Help));
        assert!(matches!(parse_input("/s"), ChatCommand::Help));
    }

    #[test]
    fn test_parse_unknown_command() {
        match parse_input("/unknown stuff") {
            ChatCommand::SendPrompt(text) => assert_eq!(text, "/unknown stuff"),
            _ => panic!("Expected SendPrompt for unknown command"),
        }
    }

    #[test]
    fn test_parse_model_show() {
        assert!(matches!(parse_input("/model"), ChatCommand::Model(None)));
        assert!(matches!(parse_input("/m"), ChatCommand::Model(None)));
    }

    #[test]
    fn test_parse_model_set() {
        match parse_input("/model sonnet") {
            ChatCommand::Model(Some(name)) => assert_eq!(name, "sonnet"),
            _ => panic!("Expected Model(Some)"),
        }
    }

    #[test]
    fn test_parse_cost() {
        assert!(matches!(parse_input("/cost"), ChatCommand::Cost));
    }

    #[test]
    fn test_parse_compact() {
        assert!(matches!(parse_input("/compact"), ChatCommand::Compact));
    }
}
