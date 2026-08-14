//! ACP Message Relay
//!
//! Handles sending messages to agents via ACP protocol.
//! Supports both reusing existing sessions and spawning new ones.

use anyhow::Result;
use tokio::sync::oneshot;
use tracing::{info, warn};

use ergatai_core::acp::manager::{manager, SessionCommand, SessionKind};
use ergatai_core::acp::sdk_session::spawn_session_task_with_kind;
use ergatai_core::agent::config::{get_agent_config, AgentConfig};

/// Send a message to an agent via ACP
///
/// Strategy:
/// 1. Try to find an existing session for the target agent
/// 2. If found, reuse it to send the message
/// 3. If not found, spawn a new session, send the message, then close it
pub async fn send_message_to_agent(
    target_agent_id: &str,
    message: &str,
) -> Result<MessageRelayResult> {
    info!("Sending message to agent {} via ACP", target_agent_id);

    // Get agent config
    let agent_config = get_agent_config(target_agent_id)?;

    // Try to find existing session for this agent
    let session_manager = manager();
    let sessions = session_manager.list_sessions().await;

    let existing_session = sessions.iter().find(|s| s.agent_name == target_agent_id);

    if let Some(session) = existing_session {
        info!("Reusing existing session {} for agent {}", session.session_id, target_agent_id);

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
        info!("No existing session for agent {}, spawning new session", target_agent_id);

        // Spawn new session, send message, then close
        let result = spawn_and_send(&agent_config, message).await?;

        Ok(MessageRelayResult {
            message_id: result.message_id,
            status: "sent".to_string(),
            session_id: result.session_id,
            session_reused: false,
            response: result.response,
        })
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

/// Spawn new session and send message
async fn spawn_and_send(agent_config: &AgentConfig, message: &str) -> Result<SendResult> {
    info!("Spawning new session for agent {}", agent_config.name);

    // Create channel to receive session ID
    let (session_id_tx, session_id_rx) = oneshot::channel();

    // Spawn the session
    ergatai_core::acp::sdk_session::spawn_session_task_with_kind(
        agent_config.clone(),
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string()),
        ergatai_core::acp::manager::SessionKind::Chat,
        session_id_tx,
    );

    // Wait for session to be ready (with timeout)
    let session_id = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        session_id_rx,
    ).await {
        Ok(Ok(Ok(id))) => {
            info!("Session spawned successfully: {}", id);
            id
        }
        Ok(Ok(Err(e))) => {
            return Err(anyhow::anyhow!("Failed to spawn session: {}", e));
        }
        Ok(Err(_)) => {
            return Err(anyhow::anyhow!("Session ID channel dropped"));
        }
        Err(_) => {
            return Err(anyhow::anyhow!("Session spawn timeout (30s)"));
        }
    };

    // Now send the message via the newly created session
    let send_result = send_via_session(&session_id, message).await;

    // Close the session after sending (optional, for now we keep it alive)
    // TODO: Implement session cleanup if needed

    match send_result {
        Ok(mut result) => {
            result.session_id = session_id.clone();
            Ok(result)
        }
        Err(e) => {
            warn!("Failed to send message to spawned session {}: {}", session_id, e);
            Err(e)
        }
    }
}

struct SendResult {
    message_id: String,
    session_id: String,
    response: Option<String>,
}
