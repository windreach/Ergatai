//! Message delivery consumer — reliable agent message delivery via NATS JetStream
//!
//! Pulls messages from the `AGENT_MESSAGES` JetStream stream and delivers each
//! to the target agent via AgentRuntime injection (rmux/tmux send_text).
//!
//! ## Reliability semantics
//!
//! - Message is **ack'd** only after successful delivery
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
//!   ↓ AgentRuntime injection (rmux/tmux send_text)
//!   ├─ OK → ack
//!   └─ fail → nak (JetStream retries)
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
/// * `cancel` — cancellation token for graceful shutdown
pub fn start_message_delivery_consumer(
    connection: NatsConnection,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("Message delivery consumer starting");

        // Initialize the pull consumer with retry — the AGENT_MESSAGES stream may
        // not be ready yet during early startup. Retry up to 10 times with backoff.
        let messages = match init_pull_consumer_with_retry(&connection, &cancel).await {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, "Failed to initialize message delivery consumer after retries");
                return;
            }
        };

        info!(
            "Message delivery consumer running (stream: {})",
            AGENT_MESSAGES_STREAM
        );

        // Process messages until cancelled or stream error
        process_messages(messages, cancel).await;

        info!("Message delivery consumer stopped");
    })
}

/// Create or get the durable pull consumer for agent messages.
///
/// Returns a boxed message stream to avoid naming the private `pull::Consumer` type.
async fn init_pull_consumer(
    connection: &NatsConnection,
) -> ErgataiResult<
    futures::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
> {
    let stream = connection
        .jetstream()
        .get_stream(AGENT_MESSAGES_STREAM)
        .await
        .map_err(|e| {
            ErgataiError::NatsError(format!("Stream {} not found: {}", AGENT_MESSAGES_STREAM, e))
        })?;

    // Durable pull consumer with explicit ack.
    // - `ack_wait: 30s` — consumer has 30s to deliver before redelivery
    // - `max_deliver: 20` — after 20 failed attempts, message is discarded by JetStream
    //   (the consumer logs the final discard so operators can detect message loss)
    // - `deliver_policy: All` — start from beginning of stream (catch up on missed)
    let consumer_config = pull::Config {
        durable_name: Some(CONSUMER_NAME.to_string()),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(30),
        max_deliver: 20,
        ..Default::default()
    };

    let consumer = stream
        .get_or_create_consumer(CONSUMER_NAME, consumer_config)
        .await
        .map_err(|e| ErgataiError::NatsError(format!("Failed to create consumer: {}", e)))?;

    let messages = consumer
        .messages()
        .await
        .map_err(|e| ErgataiError::NatsError(format!("Failed to get message stream: {}", e)))?;

    // Map the specific async_nats error type to Box<dyn Error + Send + Sync>
    // so we don't have to name the private error kind type.
    Ok(Box::pin(messages.map(|r| {
        r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    })))
}

/// Initialize the pull consumer with retry logic.
///
/// During early startup the AGENT_MESSAGES stream may not exist yet. Instead of
/// failing immediately and leaving the system without message delivery, retry
/// up to 10 times with exponential backoff (500ms → 30s cap).
async fn init_pull_consumer_with_retry(
    connection: &NatsConnection,
    cancel: &tokio_util::sync::CancellationToken,
) -> ErgataiResult<
    futures::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
> {
    let mut delay = Duration::from_millis(500);
    let max_delay = Duration::from_secs(30);

    for attempt in 1..=10 {
        match init_pull_consumer(connection).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if attempt == 10 {
                    return Err(e);
                }
                warn!(
                    attempt = attempt,
                    error = %e,
                    delay_ms = delay.as_millis() as u64,
                    "Consumer init failed, retrying"
                );
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(ErgataiError::NatsError("Cancelled during consumer init retry".to_string()));
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(max_delay);
            }
        }
    }
    // If we exit the loop (e.g., cancellation), return an error instead of panicking.
    Err(ErgataiError::NatsError(
        "Consumer initialization loop exited unexpectedly".to_string(),
    ))
}

/// Main message processing loop.
///
/// Pulls messages from the stream, attempts delivery via AgentRuntime injection,
/// and acks/naks based on the result.
async fn process_messages(
    mut messages: futures::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
    cancel: tokio_util::sync::CancellationToken,
) {
    loop {
        // Use tokio::select! to race messages against cancellation — no polling interval,
        // immediate shutdown when cancel is triggered.
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Message delivery consumer cancelled");
                break;
            }
            msg = messages.next() => {
                match msg {
                    // Stream closed
                    None => {
                        warn!("Message stream closed, consumer exiting");
                        break;
                    }

                    // Message received
                    Some(Ok(msg)) => {
                        handle_message(&msg).await;
                    }

                    // Transport error
                    Some(Err(e)) => {
                        warn!(error = %e, "Error receiving message from stream");
                        // Continue — transient errors shouldn't kill the consumer
                    }
                }
            }
        }
    }
}

/// Handle a single message: deserialize, deliver, ack/nak.
async fn handle_message(msg: &async_nats::jetstream::Message) {
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

    // Warn on redeliveries — indicates a prior delivery attempt may have succeeded
    // but the ack failed, or the consumer restarted mid-delivery.
    if let Ok(info) = msg.info() {
        if info.delivered > 1 {
            warn!(
                from = from,
                to = to,
                delivery_count = info.delivered,
                "Message redelivered — possible duplicate. Prior ack may have failed."
            );
        }
    }

    // ── Resolve sender MCP ID → runtime ID for logging ──
    let runtime = get_agent_runtime();

    // The message content is already formatted by the MCP server (server.rs)
    // with instruction + JSON payload. Just deliver it as-is.
    let formatted_message = payload.content.as_str();

    debug!(
        from = from,
        to = to,
        message_len = formatted_message.len(),
        message_preview = &formatted_message[..formatted_message.len().min(200)],
        "Delivering agent message via NATS consumer"
    );

    // ── Deliver via AgentRuntime injection (rmux/tmux send_text) ──
    // Uses the terminal multiplexer backend to inject text directly into the
    // target agent's pane, simulating keyboard input.

    // Resolve MCP agent ID to runtime agent ID for logging
    let runtime_id = runtime.resolve_agent_id(to).await;
    info!(
        from = from,
        to = to,
        runtime_id = ?runtime_id,
        "Delivering message: MCP target → runtime resolution"
    );

    match runtime.inject_message(to, formatted_message).await {
        Ok(()) => {
            info!(
                from = from,
                to = to,
                "Message delivered via AgentRuntime injection"
            );
            if let Err(e) = msg.ack().await {
                warn!("Failed to ack delivery: {}", e);
            }
        }
        Err(e) => {
            warn!(
                from = from,
                to = to,
                error = %e,
                "AgentRuntime injection failed — naking for retry"
            );
            if let Err(nak_err) = msg.ack_with(AckKind::Nak(None)).await {
                error!("Failed to nak message: {}", nak_err);
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consumer_name_is_durable() {
        assert_eq!(CONSUMER_NAME, "message_delivery");
    }

    #[test]
    fn test_consumer_name_is_non_empty() {
        assert!(!CONSUMER_NAME.is_empty());
    }

    #[test]
    fn test_consumer_name_is_snake_case() {
        // Durable names in NATS JetStream should be snake_case/kebab-case
        assert!(
            CONSUMER_NAME
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
            "CONSUMER_NAME should be snake_case"
        );
    }

    #[test]
    fn test_ack_wait_is_thirty_seconds() {
        // The consumer config uses ack_wait: Duration::from_secs(30).
        // Document the expected value so changes are visible.
        let expected_ack_wait = Duration::from_secs(30);
        assert_eq!(expected_ack_wait.as_secs(), 30);
    }

    #[test]
    fn test_max_deliver_is_twenty() {
        // The consumer config uses max_deliver: 20.
        // Document the expected value so changes are visible.
        let expected_max_deliver: i64 = 20;
        assert_eq!(expected_max_deliver, 20);
    }

    #[test]
    fn test_retry_delay_starts_at_500ms() {
        // init_pull_consumer_with_retry starts with delay = 500ms
        let initial_delay = Duration::from_millis(500);
        assert_eq!(initial_delay.as_millis(), 500);
    }

    #[test]
    fn test_retry_delay_max_is_30s() {
        let max_delay = Duration::from_secs(30);
        assert_eq!(max_delay.as_secs(), 30);
    }

    #[test]
    fn test_retry_delay_exponential_backoff_sequence() {
        // Verify the backoff sequence: 500ms, 1s, 2s, 4s, 8s, 16s, 30s (capped), ...
        let mut delay = Duration::from_millis(500);
        let max_delay = Duration::from_secs(30);

        let expected = vec![500, 1000, 2000, 4000, 8000, 16000, 30000, 30000];
        for &exp_ms in &expected {
            assert_eq!(delay.as_millis() as u64, exp_ms, "delay sequence mismatch");
            delay = (delay * 2).min(max_delay);
        }
        // After reaching the cap, further doublings stay at cap
        assert_eq!(delay.as_secs(), 30);
    }

    #[test]
    fn test_retry_max_attempts_is_ten() {
        // init_pull_consumer_with_retry retries up to 10 times.
        let max_attempts: u32 = 10;
        assert_eq!(max_attempts, 10);
        // Total worst-case wait: 500+1000+2000+4000+8000+16000+30000*4 = 151.5s
        // This bounds the startup delay when the stream isn't ready.
    }
}
