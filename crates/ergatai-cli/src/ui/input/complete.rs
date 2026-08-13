//! Slash-command autocomplete.
//!
//! When the user types a `/` at the start of the input, [`SlashCompleter`]
//! produces a list of matching commands. The TUI renders these as a popup
//! above the input area; `Tab` / `↓` move the selection, `Enter` accepts.

/// A single completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The full label (e.g. `/help`). Inserted into the input on accept.
    pub label: String,
    /// One-line description shown beside the label.
    pub description: String,
}

/// Stateless slash-command completer.
pub struct SlashCompleter;

impl SlashCompleter {
    /// Return all commands whose label starts with `input`.
    ///
    /// Returns an empty vec when `input` does not start with `/` — the caller
    /// should hide the popup in that case.
    pub fn complete(input: &str) -> Vec<Completion> {
        let trimmed = input.trim_start();
        if !trimmed.starts_with('/') {
            return vec![];
        }
        // Only autocomplete while the user is still typing the command token
        // (no space yet, or only one token).
        if trimmed.contains(' ') {
            return vec![];
        }

        let all: Vec<(&str, &str)> = vec![
            ("help", "Show help message"),
            ("quit", "Exit the chat"),
            ("clear", "Clear the messages pane"),
            ("agents", "List available agents"),
            ("switch", "Switch to a different agent (/switch <name>)"),
            ("status", "Show current status"),
            ("model", "Show or set the model (/model [name])"),
            ("cost", "Show session cost and token usage"),
            ("compact", "Compact the conversation context"),
        ];

        all.into_iter()
            .filter(|(name, _)| format!("/{name}").starts_with(trimmed))
            .map(|(n, d)| Completion {
                label: format!("/{n}"),
                description: d.to_string(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_slash_returns_empty() {
        assert!(SlashCompleter::complete("hello").is_empty());
        assert!(SlashCompleter::complete("").is_empty());
    }

    #[test]
    fn test_slash_alone_returns_all() {
        let results = SlashCompleter::complete("/");
        assert_eq!(results.len(), 9);
    }

    #[test]
    fn test_prefix_filter() {
        let results = SlashCompleter::complete("/h");
        let labels: Vec<&str> = results.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"/help"));
        // Must NOT contain commands that don't start with /h.
        assert!(!labels.contains(&"/quit"));
        assert!(!labels.contains(&"/clear"));
    }

    #[test]
    fn test_exact_match() {
        let results = SlashCompleter::complete("/help");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "/help");
    }

    #[test]
    fn test_space_dismisses() {
        // Once the user typed a space we're past the command token.
        assert!(SlashCompleter::complete("/help me").is_empty());
    }

    #[test]
    fn test_mo_matches_model() {
        let results = SlashCompleter::complete("/mo");
        let labels: Vec<&str> = results.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"/model"));
    }
}
