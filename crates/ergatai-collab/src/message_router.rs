//! Agent message router — detects @agent mentions and routes via NATS
//!
//! Enables bidirectional communication between agents through Ergatai as relay.
//! When an agent's output contains `@target_agent`, the router:
//! 1. Extracts the mention
//! 2. Publishes an AgentMessagePayload to NATS
//! 3. The target agent receives the message via tmux injection or MCP notification

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use std::sync::LazyLock;
use tracing::{debug, info, warn};

use ergatai_error::ErgataiResult;
use ergatai_nats::{get_nats_connection, AgentMessagePayload, EventBus};

// Compile regex once at startup.
// Match @agent only at start of line or after whitespace.
// This prevents false positives on email addresses (user@example.com).
// Note: Rust's regex crate does not support look-behind, so we consume the
// leading whitespace in the match. Capture group 1 still holds only the name.
static MENTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)(?:^|\s)@([a-zA-Z0-9_-]+)").expect("valid regex"));

/// Detect @agent mentions in text
///
/// Returns a deduplicated list of agent names mentioned (without the @ prefix).
/// Only matches @ at word boundaries (start of line or after whitespace) to
/// avoid false positives on email addresses and URLs.
/// Example: "@codex please review" → ["codex"]
pub fn extract_mentions(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    MENTION_RE
        .captures_iter(text)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// Route a message from one agent to another via NATS
///
/// # Arguments
/// * `from_agent` - Source agent name
/// * `to_agent` - Target agent name
/// * `content` - Message content
/// * `thread_id` - Optional conversation thread ID
pub async fn route_agent_message(
    from_agent: &str,
    to_agent: &str,
    content: &str,
    thread_id: Option<String>,
) -> ErgataiResult<()> {
    let conn = get_nats_connection().await.ok_or_else(|| {
        warn!("NATS connection not available, cannot route agent message");
        ergatai_error::ErgataiError::internal("NATS connection not available")
    })?;

    let bus = EventBus::new(conn);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Look up agent UUIDs for stable routing
    let runtime = ergatai_runtime::get_agent_runtime();
    let from_uuid = runtime
        .get_agent(from_agent)
        .await
        .map(|info| info.agent_uuid);
    let to_uuid = runtime
        .get_agent(to_agent)
        .await
        .map(|info| info.agent_uuid);

    let payload = AgentMessagePayload {
        from_agent: from_agent.to_string(),
        to_agent: to_agent.to_string(),
        from_uuid,
        to_uuid,
        content: content.to_string(),
        thread_id,
        timestamp,
        metadata: HashMap::new(),
    };

    bus.publish_agent_message(&payload).await?;

    info!(
        from = from_agent,
        to = to_agent,
        "Routed agent message via NATS"
    );

    Ok(())
}

/// Scan agent output for @mentions and route them
///
/// Called after an agent completes a task. If the output contains @agent_name,
/// automatically route the message to that agent.
///
/// # Arguments
/// * `from_agent` - Agent that produced the output
/// * `output` - Agent's output text
/// * `thread_id` - Optional thread ID for multi-turn conversations
pub async fn scan_and_route_mentions(
    from_agent: &str,
    output: &str,
    thread_id: Option<String>,
) -> ErgataiResult<usize> {
    let mentions = extract_mentions(output);

    if mentions.is_empty() {
        debug!(agent = from_agent, "No @mentions found in output");
        return Ok(0);
    }

    info!(
        agent = from_agent,
        mentions = mentions.len(),
        "Found @mentions in agent output"
    );

    let mut routed_count = 0;
    for to_agent in mentions {
        // Don't route messages to self
        if to_agent == from_agent {
            continue;
        }

        match route_agent_message(from_agent, &to_agent, output, thread_id.clone()).await {
            Ok(_) => routed_count += 1,
            Err(e) => {
                warn!(
                    from = from_agent,
                    to = to_agent,
                    error = %e,
                    "Failed to route agent message"
                );
            }
        }
    }

    Ok(routed_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_mentions_single() {
        let text = "@codex please review this code";
        let mentions = extract_mentions(text);
        assert_eq!(mentions, vec!["codex"]);
    }

    #[test]
    fn test_extract_mentions_multiple() {
        let text = "@claude-code and @codex please collaborate";
        let mentions = extract_mentions(text);
        assert_eq!(mentions.len(), 2);
        assert!(mentions.contains(&"claude-code".to_string()));
        assert!(mentions.contains(&"codex".to_string()));
    }

    #[test]
    fn test_extract_mentions_with_dashes() {
        let text = "@my-agent please help";
        let mentions = extract_mentions(text);
        assert_eq!(mentions, vec!["my-agent"]);
    }

    #[test]
    fn test_extract_mentions_with_underscores() {
        let text = "@my_agent please help";
        let mentions = extract_mentions(text);
        assert_eq!(mentions, vec!["my_agent"]);
    }

    #[test]
    fn test_extract_mentions_none() {
        let text = "no mentions here";
        let mentions = extract_mentions(text);
        assert!(mentions.is_empty());
    }

    #[test]
    fn test_extract_mentions_at_end() {
        let text = "please help @agent";
        let mentions = extract_mentions(text);
        assert_eq!(mentions, vec!["agent"]);
    }

    #[test]
    fn test_extract_mentions_adjacent() {
        let text = "@agent1@agent2";
        let mentions = extract_mentions(text);
        // Only match the first one (the second @ is not preceded by whitespace or start-of-line)
        // This is the correct behavior to avoid matching email addresses
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0], "agent1");
    }
}
