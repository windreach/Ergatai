//! ACP Message Relay
//!
//! Handles sending messages to agents via ACP protocol.
//! In middleware mode, agents are already running and connected via MCP.

use anyhow::Result;
use tokio::sync::oneshot;
use tracing::{info, warn};

use ergatai_core::acp::manager::{manager, SessionCommand};

/// Send a message to an agent via ACP
///
/// In the middleware architecture:
/// - Agents connect to Ergatai via MCP
/// - Agents expose ACP server endpoints
/// - Ergatai forwards messages via ACP
///
/// This function looks up the agent's ACP connection and sends the message.
pub async fn send_message_to_agent(
    target_agent_id: &str,
    message: &str,
) -> Result<MessageRelayResult> {
    info!("Sending message to agent {} via ACP", target_agent_id);

    // Get ACP connection for the target agent
    // In middleware mode, agents register their ACP endpoint when connecting via MCP
    let session_manager = manager();
    let sessions = session_manager.list_sessions().await;

    let existing_session = sessions.iter().find(|s| s.agent_name == target_agent_id);

    if let Some(session) = existing_session {
        info!("Found ACP session {} for agent {}", session.session_id, target_agent_id);

        // Send message via existing session
        let result = send_via_session(&session.session_id, message).await?;

        Ok(MessageRelayResult {
            message_id: result.message_id,
            status: "sent".to_string(),
            session_id: session.session_id.clone(),
            session_reused: true,
            response: result.response,
        })
    } else {
        // No existing ACP session - agent needs to connect first
        warn!("No ACP session found for agent {}. Agent must connect via MCP first.", target_agent_id);

        Err(anyhow::anyhow!(
            "Agent {} is not connected. Agents must connect via MCP before receiving messages.",
            target_agent_id
        ))
    }
}

/// Result of message relay
pub struct MessageRelayResult {
    pub message_id: String,
    pub status: String,
    pub session_id: String,
    pub session_reused: bool,
    pub response: Option<String>,
}

/// Send message via existing session
async fn send_via_session(session_id: &str, message: &str) -> Result<SendResult> {
    let session_manager = manager();
    let cmd_tx = session_manager
        .get_cmd_tx(session_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("Session {} not found", session_id))?;

    // Create oneshot channel for reply
    let (reply_tx, reply_rx) = oneshot::channel();

    // Send prompt command
    cmd_tx.send(SessionCommand::SendPrompt {
        text: message.to_string(),
        reply_tx,
    })?;

    // Wait for reply (with timeout)
    let timeout_duration = std::time::Duration::from_secs(300); // 5 minutes
    match tokio::time::timeout(timeout_duration, reply_rx).await {
        Ok(Ok(result)) => {
            result?;
            Ok(SendResult {
                message_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                response: Some("Message sent successfully".to_string()),
            })
        }
        Ok(Err(e)) => {
            warn!("Send failed: {}", e);
            Err(anyhow::anyhow!("Send failed: {}", e))
        }
        Err(_) => {
            warn!("Send timeout after 5 minutes");
            Err(anyhow::anyhow!("Send timeout"))
        }
    }
}

struct SendResult {
    message_id: String,
    session_id: String,
    response: Option<String>,
}
