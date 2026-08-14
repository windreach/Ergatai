//! NATS → ACP Message Forwarder
//!
//! Subscribes to NATS agent messages and forwards them to agents via ACP protocol.
//! This completes the message routing loop:
//! Agent A → send_message (MCP) → NATS → this forwarder → Agent B (ACP)

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use ergatai_acp::agent_registry::AgentRegistry;
use ergatai_nats::{AgentMessagePayload, EventBus};
use futures::StreamExt;
use reqwest::Client;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Shared HTTP client for forwarding messages (reused across all forwards)
static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client")
});

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
/// Uses direct HTTP POST to the agent's /acp endpoint.
/// This is simpler than using the full ACP SDK client for basic message forwarding.
async fn forward_via_acp(
    payload: &AgentMessagePayload,
    acp_endpoint: &str,
) -> anyhow::Result<()> {
    // Format the message content
    let message_text = format!(
        "来自 {} 的消息:\n\n{}",
        payload.from_agent, payload.content
    );

    info!("Forwarding to {} via HTTP POST to {}/acp", payload.to_agent, acp_endpoint);

    // Use the shared HTTP client (with timeout configured)
    let client = &HTTP_CLIENT;

    // For simplicity, we use a deterministic session ID based on the agent
    // This allows agents to maintain state per-sender if needed
    // In production, should use proper session lifecycle management
    let session_id = format!("forwarded-{}", payload.from_agent);

    // Build session/prompt request
    let prompt_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [
                {
                    "type": "text",
                    "text": message_text
                }
            ]
        }
    });

    // Send to agent's ACP endpoint
    let response = client
        .post(format!("{}/acp", acp_endpoint))
        .header("Content-Type", "application/json")
        .json(&prompt_request)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        anyhow::bail!("Agent returned error status {}: {}", status, body);
    }

    info!("Message forwarded to {} successfully (status: {})", payload.to_agent, status);
    Ok(())
}
