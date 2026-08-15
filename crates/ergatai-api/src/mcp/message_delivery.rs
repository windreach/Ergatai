//! Message delivery consumer — reliable agent message delivery via NATS JetStream
//!
//! Pulls messages from the `AGENT_MESSAGES` JetStream stream and delivers each
//! to the target agent via (in order):
//! 1. **AgentRuntime injection** — preferred, uses backend (tmux) or MCP fallback
//! 2. **MCP custom notification** — fallback when backend injection unavailable
//!
//! ## Reliability semantics
//!
//! - Message is **ack'd** only after successful delivery (runtime or MCP)
//! - On delivery failure: message is **nak'd** → JetStream redelivers after `ack_wait`
//! - After `max_deliver` attempts (set on the consumer): message is discarded
//! - Stream retention is `WorkQueue`: ack'd messages are removed immediately
//!
//! ## Flow
//!
//! ```text
//! AGENT_MESSAGES stream (JetStream, file-backed)
//!   ↓ pull
//! MessageDeliveryConsumer
//!   ↓ deserialize AgentMessagePayload
//!   ↓ resolve target agent (runtime or MCP peer)
//!   ├─ AgentRuntime injection OK → ack
//!   ├─ MCP notification OK → ack
//!   └─ both fail → nak (JetStream retries)
//! ```

use std::time::Duration;

use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use async_nats::jetstream::message::AckKind;
use futures::StreamExt;
use tracing::{debug, error, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};
use ergatai_nats::connection::NatsConnection;
use ergatai_nats::events::AgentMessagePayload;
use ergatai_nats::AGENT_MESSAGES_STREAM;
use ergatai_runtime::get_agent_runtime;

use super::server::PeerRegistry;

/// Consumer name for the message delivery pull consumer.
/// Durable — survives consumer restarts and resumes from last ack.
const CONSUMER_NAME: &str = "message_delivery";

/// Start the message delivery consumer as a background task.
///
/// Returns a `JoinHandle` for the spawned task. The task runs until the
/// cancellation token is triggered or the NATS stream becomes unavailable.
///
/// # Arguments
/// * `connection` — NATS connection (must be initialized with JetStream)
/// * `peer_registry` — MCP peer registry for notification fallback
/// * `cancel` — cancellation token for graceful shutdown
pub fn start_message_delivery_consumer(
    connection: NatsConnection,
    peer_registry: PeerRegistry,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("Message delivery consumer starting");

        // Initialize the pull consumer inline (pull::Consumer is private,
        // so we cannot name its type in a function signature).
        let messages = match init_pull_consumer(&connection).await {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, "Failed to initialize message delivery consumer");
                return;
            }
        };

        info!("Message delivery consumer running (stream: {})", AGENT_MESSAGES_STREAM);

        // Process messages until cancelled or stream error
        process_messages(messages, peer_registry, cancel).await;

        info!("Message delivery consumer stopped");
    })
}

/// Create or get the durable pull consumer for agent messages.
///
/// Returns a boxed message stream to avoid naming the private `pull::Consumer` type.
async fn init_pull_consumer(
    connection: &NatsConnection,
) -> ErgataiResult<futures::stream::BoxStream<'static, Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>>> {
    let stream = connection
        .jetstream()
        .get_stream(AGENT_MESSAGES_STREAM)
        .await
        .map_err(|e| {
            ErgataiError::NatsError(format!(
                "Stream {} not found: {}",
                AGENT_MESSAGES_STREAM, e
            ))
        })?;

    // Durable pull consumer with explicit ack.
    // - `ack_wait: 30s` — consumer has 30s to deliver before redelivery
    // - `max_deliver: 5` — after 5 failed attempts, message is discarded
    // - `deliver_policy: All` — start from beginning of stream (catch up on missed)
    let consumer_config = pull::Config {
        durable_name: Some(CONSUMER_NAME.to_string()),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(30),
        max_deliver: 5,
        ..Default::default()
    };

    let consumer = stream
        .get_or_create_consumer(CONSUMER_NAME, consumer_config)
        .await
        .map_err(|e| {
            ErgataiError::NatsError(format!("Failed to create consumer: {}", e))
        })?;

    let messages = consumer.messages().await.map_err(|e| {
        ErgataiError::NatsError(format!("Failed to get message stream: {}", e))
    })?;

    // Map the specific async_nats error type to Box<dyn Error + Send + Sync>
    // so we don't have to name the private error kind type.
    Ok(Box::pin(messages.map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>))))
}

/// Main message processing loop.
///
/// Pulls messages from the stream, attempts delivery via AgentRuntime then MCP,
/// and acks/naks based on the result.
async fn process_messages(
    mut messages: futures::stream::BoxStream<'static, Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>>,
    peer_registry: PeerRegistry,
    cancel: tokio_util::sync::CancellationToken,
) {
    loop {
        // Use timeout to allow periodic cancellation checks
        let message = tokio::time::timeout(Duration::from_secs(5), messages.next()).await;

        match message {
            // Timeout — no message within 5s, loop to check cancellation
            Err(_) => {
                if cancel.is_cancelled() {
                    info!("Message delivery consumer cancelled (idle timeout)");
                    break;
                }
                continue;
            }

            // Stream closed
            Ok(None) => {
                warn!("Message stream closed, consumer exiting");
                break;
            }

            // Message received
            Ok(Some(Ok(msg))) => {
                if cancel.is_cancelled() {
                    // Nak so it's redelivered after shutdown
                    if let Err(e) = msg.ack_with(AckKind::Nak(None)).await {
                        warn!("Failed to nak message during shutdown: {}", e);
                    }
                    break;
                }

                handle_message(&msg, &peer_registry).await;
            }

            // Transport error
            Ok(Some(Err(e))) => {
                warn!(error = %e, "Error receiving message from stream");
                // Continue — transient errors shouldn't kill the consumer
            }
        }
    }
}

/// Handle a single message: deserialize, deliver, ack/nak.
async fn handle_message(
    msg: &async_nats::jetstream::Message,
    peer_registry: &PeerRegistry,
) {
    // Deserialize the payload
    let payload: AgentMessagePayload = match serde_json::from_slice(&msg.payload) {
        Ok(p) => p,
        Err(e) => {
            error!(
                error = %e,
                subject = msg.subject.as_str(),
                "Failed to deserialize AgentMessagePayload — acking to discard"
            );
            // Malformed message — ack to discard (retry won't help)
            if let Err(ack_err) = msg.ack().await {
                warn!("Failed to ack malformed message: {}", ack_err);
            }
            return;
        }
    };

    let from = &payload.from_agent;
    let to = &payload.to_agent;

    debug!(
        from = from,
        to = to,
        "Delivering agent message via NATS consumer"
    );

    // Format the message for display in the target agent's pane
    let formatted_message = format!("Message from {}: {}", from, payload.content);

    // ── Attempt 1: AgentRuntime injection (preferred) ──
    // This tries backend injection (tmux) first, then falls back to MCP notification
    let runtime = get_agent_runtime();
    match runtime.inject_message(to, &formatted_message).await {
        Ok(()) => {
            info!(from = from, to = to, "Message delivered via AgentRuntime");
            if let Err(e) = msg.ack().await {
                warn!("Failed to ack runtime delivery: {}", e);
            }
            return;
        }
        Err(e) => {
            debug!(
                from = from,
                to = to,
                error = %e,
                "AgentRuntime injection failed, trying direct MCP notification"
            );
        }
    }

    // ── Attempt 2: Direct MCP custom notification (fallback) ──
    // This is used when the target agent is not tracked by AgentRuntime
    // but has a direct MCP connection
    match send_mcp_notification(to, &payload, peer_registry).await {
        Ok(()) => {
            info!(from = from, to = to, "Message delivered via MCP notification");
            if let Err(e) = msg.ack().await {
                warn!("Failed to ack MCP delivery: {}", e);
            }
        }
        Err(e) => {
            warn!(
                from = from,
                to = to,
                error = %e,
                "Both delivery methods failed — naking for retry"
            );
            if let Err(nak_err) = msg.ack_with(AckKind::Nak(None)).await {
                error!("Failed to nak message: {}", nak_err);
            }
        }
    }
}

/// Send MCP custom notification to the target agent.
async fn send_mcp_notification(
    target_agent_id: &str,
    payload: &AgentMessagePayload,
    peer_registry: &PeerRegistry,
) -> ErgataiResult<()> {
    let peer = {
        let peers = peer_registry.read().await;
        peers.get(target_agent_id).cloned()
    };

    let peer = match peer {
        Some(p) => p,
        None => {
            return Err(ErgataiError::internal(format!(
                "Agent {} has no active MCP connection",
                target_agent_id
            )));
        }
    };

    let notification_payload = serde_json::json!({
        "from_agent": payload.from_agent,
        "to_agent": payload.to_agent,
        "content": payload.content,
        "message_type": "request",
        "timestamp": payload.timestamp,
    });

    let notification =
        rmcp::model::CustomNotification::new("ergatai/message", Some(notification_payload));

    peer.send_notification(rmcp::model::ServerNotification::CustomNotification(notification))
        .await
        .map_err(|e| {
            ErgataiError::internal(format!(
                "MCP notification to {} failed: {}",
                target_agent_id, e
            ))
        })?;

    Ok(())
}
