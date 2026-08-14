//! NATS → ACP Message Forwarder
//!
//! Subscribes to NATS agent messages and forwards them to agents via ACP protocol.
//! This completes the message routing loop:
//! Agent A → send_message (MCP) → NATS → this forwarder → Agent B (ACP)

use std::sync::Arc;

use ergatai_acp::agent_registry::AgentRegistry;
use ergatai_acp::http_client::http_connection_manager;
use ergatai_acp::manager::SessionKind;
use ergatai_nats::{AgentMessagePayload, EventBus};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Start the NATS → ACP message forwarder as a background task.
///
/// This subscribes to all agent messages on NATS and forwards them
/// to the target agent via standard ACP protocol.
pub fn start_nats_acp_forwarder(
    registry: Arc<AgentRegistry>,
    cancellation_token: CancellationToken,
) {
    tokio::spawn(async move {
        info!("Starting NATS → ACP message forwarder");

        // Wait for NATS to be initialized
        while !ergatai_nats::is_nats_initialized().await {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            if cancellation_token.is_cancelled() {
                info!("NATS forwarder cancelled during initialization");
                return;
            }
        }

        let conn = match ergatai_nats::get_nats_connection().await {
            Some(c) => c,
            None => {
                error!("NATS connection not available even though is_nats_initialized() returned true");
                return;
            }
        };

        let bus = EventBus::new(conn);

        // Subscribe to all agent messages
        let mut subscriber = match bus.subscribe_all_agent_messages().await {
            Ok(sub) => {
                info!("Subscribed to NATS agent messages (ergatai.agent.message.*)");
                sub
            }
            Err(e) => {
                error!("Failed to subscribe to NATS agent messages: {}", e);
                return;
            }
        };

        // Process messages
        loop {
            tokio::select! {
                msg = subscriber.next() => {
                    match msg {
                        Some(msg) => {
                            handle_nats_message(&registry, msg).await;
                        }
                        None => {
                            warn!("NATS subscriber ended unexpectedly");
                            break;
                        }
                    }
                }
                _ = cancellation_token.cancelled() => {
                    info!("NATS forwarder shutting down");
                    break;
                }
            }
        }
    });
}

/// Handle a single NATS message by forwarding it to the target agent via ACP.
async fn handle_nats_message(
    registry: &AgentRegistry,
    msg: async_nats::Message,
) {
    // Parse the message payload
    let payload: AgentMessagePayload = match serde_json::from_slice(&msg.payload) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse NATS message payload: {}", e);
            return;
        }
    };

    info!(
        "Received NATS message: from={}, to={}, subject={}",
        payload.from_agent, payload.to_agent, msg.subject
    );

    // Look up the target agent's ACP endpoint
    let acp_endpoint = match registry.get_acp_endpoint(&payload.to_agent).await {
        Some(endpoint) => endpoint,
        None => {
            warn!(
                "Agent {} has no ACP endpoint registered. Message from {} cannot be delivered.",
                payload.to_agent, payload.from_agent
            );
            return;
        }
    };

    info!(
        "Forwarding message to {} at {} via ACP",
        payload.to_agent, acp_endpoint
    );

    // Forward message via ACP protocol
    if let Err(e) = forward_via_acp(&payload, &acp_endpoint).await {
        error!("Failed to forward message to {}: {}", payload.to_agent, e);
    }
}

/// Forward a message to an agent via ACP protocol.
///
/// Uses the HttpConnectionManager to maintain persistent connections.
/// If not already connected, creates a new connection.
async fn forward_via_acp(
    payload: &AgentMessagePayload,
    acp_endpoint: &str,
) -> anyhow::Result<()> {
    let manager = http_connection_manager();

    // Format the message content
    let message_text = format!(
        "来自 {} 的消息:\n\n{}",
        payload.from_agent, payload.content
    );

    // Try to send prompt to the agent
    // If not connected, this will fail - we need to connect first
    match manager.send_prompt(&payload.to_agent, message_text.clone()).await {
        Ok(_) => {
            info!("Message forwarded to {} successfully", payload.to_agent);
            Ok(())
        }
        Err(_) => {
            // Not connected yet, try to connect
            info!("Not connected to {}, establishing ACP connection", payload.to_agent);

            // Use a default working directory for forwarded messages
            // TODO: Store agent's preferred cwd in registry during set_acp_endpoint
            let cwd = std::env::var("ERGATAI_DEFAULT_CWD")
                .or_else(|_| std::env::current_dir().map(|p| p.to_string_lossy().to_string()))
                .unwrap_or_else(|_| ".".to_string());

            // Connect to the agent
            manager.connect(
                &payload.to_agent,
                acp_endpoint,
                cwd,
                SessionKind::Chat,
            ).await?;

            info!("Connected to {}, sending message", payload.to_agent);

            // Now send the message
            manager.send_prompt(&payload.to_agent, message_text).await?;

            info!("Message forwarded to {} successfully", payload.to_agent);
            Ok(())
        }
    }
}
