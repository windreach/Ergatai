//! ACP Message Relay (HTTP-based)
//!
//! In middleware mode, agents are already running and expose ACP HTTP endpoints.
//! This module uses the HTTP ACP client to send messages to agents.
//!
//! Flow:
//! 1. Agent connects to Ergatai via MCP (tool calls)
//! 2. Agent registers its ACP endpoint (e.g., "http://localhost:8080")
//! 3. Ergatai uses HttpAcpClient to push messages/tasks to the agent
//! 4. Agent processes the message and returns the result

use anyhow::Result;
use tracing::info;

use ergatai_acp::http_client::http_connection_manager;
use ergatai_acp::agent_registry::AgentRegistry;
use ergatai_core::acp::manager::SessionKind;

/// Send a message to an agent via ACP HTTP.
///
/// In the middleware architecture:
/// - Agents connect to Ergatai via MCP and register their ACP endpoint
/// - Ergatai uses HTTP ACP client to push messages to the agent
/// - The agent processes the message and returns the result
///
/// # Arguments
/// * `target_agent_id` - The agent to send the message to
/// * `message` - The message to send
/// * `registry` - Agent registry to look up the ACP endpoint
/// * `cwd` - Working directory for the session (used if creating a new connection)
#[allow(dead_code)] // Reserved for future message relay functionality
pub async fn send_message_to_agent(
    target_agent_id: &str,
    message: &str,
    registry: &AgentRegistry,
    cwd: &str,
) -> Result<MessageRelayResult> {
    info!("Sending message to agent {} via ACP HTTP", target_agent_id);

    // Look up the agent's ACP endpoint from the registry
    let acp_endpoint = registry
        .get_acp_endpoint(target_agent_id)
        .await
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Agent {} has no ACP endpoint registered. \
                 Agents must register their ACP endpoint via ergatai.set_acp_endpoint tool.",
                target_agent_id
            )
        })?;

    info!("Agent {} ACP endpoint: {}", target_agent_id, acp_endpoint);

    // Check if we already have a connection to this agent
    let manager = http_connection_manager();
    let is_connected = manager.is_connected(target_agent_id).await;

    let session_id = if is_connected {
        info!("Reusing existing connection to agent {}", target_agent_id);
        // Get the session ID from the existing connection
        // For now, we'll use the agent_id as the key since HttpConnectionManager
        // stores connections by agent_id
        format!("session-{}", target_agent_id)
    } else {
        // Establish a new HTTP ACP connection to the agent
        info!(
            "Establishing new ACP connection to agent {} at {}",
            target_agent_id, acp_endpoint
        );

        manager
            .connect(
                target_agent_id,
                &acp_endpoint,
                cwd.to_string(),
                SessionKind::Chat, // Default to Chat; could be parameterized
            )
            .await?
    };

    // Send the message via the HTTP connection
    manager.send_prompt(target_agent_id, message.to_string()).await?;

    Ok(MessageRelayResult {
        message_id: uuid::Uuid::new_v4().to_string(),
        status: "sent".to_string(),
        session_id,
        session_reused: is_connected,
        response: Some("Message sent successfully via ACP HTTP".to_string()),
    })
}

/// Result of message relay
#[allow(dead_code)] // Reserved for future message relay functionality
pub struct MessageRelayResult {
    pub message_id: String,
    pub status: String,
    pub session_id: String,
    pub session_reused: bool,
    pub response: Option<String>,
}

/// Disconnect from an agent.
///
/// Called when an agent disconnects or is no longer available.
#[allow(dead_code)] // Reserved for future connection management
pub async fn disconnect_agent(agent_id: &str) -> Result<()> {
    let manager = http_connection_manager();
    if manager.is_connected(agent_id).await {
        info!("Disconnecting from agent {} via ACP HTTP", agent_id);
        manager.disconnect(agent_id).await?;
    }
    Ok(())
}

/// List all active ACP HTTP connections.
#[allow(dead_code)] // Reserved for future connection listing API
pub async fn list_connections() -> Vec<(String, String)> {
    let manager = http_connection_manager();
    manager.list_connections().await
}
