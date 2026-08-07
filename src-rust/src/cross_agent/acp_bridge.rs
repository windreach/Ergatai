// ACP Bridge - Detect cross-agent intent in messages

/// Detect if a message is intended for another agent
/// Returns Some(target_agent_id) if the message is for another agent
pub fn detect_cross_agent_intent(content: &str) -> Option<String> {
    // Simple pattern matching for now
    // Examples:
    // "@codex please review this code"
    // "send to claude: ..."
    // "ask opencode to ..."

    let content_lower = content.to_lowercase();

    // Check for @mention pattern
    if let Some(at_pos) = content_lower.find('@') {
        let after_at = &content_lower[at_pos + 1..];
        let agent_name = after_at
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(|c: char| !c.is_alphanumeric());

        if !agent_name.is_empty() {
            return Some(agent_name.to_string());
        }
    }

    // Check for "send to X" pattern
    if content_lower.contains("send to ") {
        if let Some(pos) = content_lower.find("send to ") {
            let after = &content_lower[pos + 8..];
            let agent_name = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(|c: char| !c.is_alphanumeric());

            if !agent_name.is_empty() {
                return Some(agent_name.to_string());
            }
        }
    }

    // Check for "ask X to" pattern
    if content_lower.contains("ask ") && content_lower.contains(" to ") {
        if let Some(pos) = content_lower.find("ask ") {
            let after = &content_lower[pos + 4..];
            let agent_name = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(|c: char| !c.is_alphanumeric());

            if !agent_name.is_empty() {
                return Some(agent_name.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_cross_agent_intent() {
        assert_eq!(
            detect_cross_agent_intent("@codex please review this"),
            Some("codex".to_string())
        );

        assert_eq!(
            detect_cross_agent_intent("send to claude: hello"),
            Some("claude".to_string())
        );

        assert_eq!(
            detect_cross_agent_intent("ask opencode to help"),
            Some("opencode".to_string())
        );

        assert_eq!(detect_cross_agent_intent("hello world"), None);
    }
}
