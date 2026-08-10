//! File events consumer for handling file.ready and file.error events
//!
//! Phase 7: Consumes events from the FILE_EVENTS JetStream stream and
//! notifies waiters when files become ready or when errors occur.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy, PullConsumer};
use tracing::{info, debug, warn, error};

use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::lock_manager::FileLockManager;
use crate::nats::connection::NatsConnection;
use crate::nats::events::FileErrorPayload;

/// Consumer for file events (ready/error)
pub struct FileEventsConsumer {
    connection: NatsConnection,
    stream_name: String,
    consumer_name: String,
    lock_manager: Arc<FileLockManager>,
}

impl FileEventsConsumer {
    /// Create a new file events consumer
    ///
    /// # Arguments
    ///
    /// * `connection` - NATS connection
    /// * `stream_name` - JetStream stream name (should be "FILE_EVENTS")
    /// * `consumer_name` - Consumer group name
    /// * `lock_manager` - FileLockManager for notifying waiters
    pub async fn new(
        connection: NatsConnection,
        stream_name: String,
        consumer_name: String,
        lock_manager: Arc<FileLockManager>,
    ) -> ErgataiResult<Self> {
        info!(
            stream = stream_name,
            consumer = consumer_name,
            "File events consumer created"
        );

        Ok(Self {
            connection,
            stream_name,
            consumer_name,
            lock_manager,
        })
    }

    /// Start consuming file events in a background task
    ///
    /// This will continuously poll for new events and process them.
    /// Returns a handle that can be used to stop the consumer.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("File events consumer started");

            // Create consumer and messages stream ONCE (not per iteration)
            let messages_result = async {
                let stream = self.connection.jetstream().get_stream(&self.stream_name).await
                    .map_err(|e| ErgataiError::NatsError(format!("Stream {} not found: {}", self.stream_name, e)))?;

                let consumer_config = pull::Config {
                    durable_name: Some(self.consumer_name.clone()),
                    deliver_policy: DeliverPolicy::All,
                    ack_policy: AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(30),
                    max_deliver: 3,
                    ..Default::default()
                };

                let consumer = stream.get_or_create_consumer(&self.consumer_name, consumer_config).await
                    .map_err(|e| ErgataiError::NatsError(format!("Failed to create consumer: {}", e)))?;

                let messages = consumer.messages().await
                    .map_err(|e| ErgataiError::NatsError(format!("Failed to get messages: {}", e)))?;

                Ok::<_, ErgataiError>(messages)
            }.await;

            let mut messages = match messages_result {
                Ok(m) => m,
                Err(e) => {
                    error!(error = %e, "Failed to initialize file events consumer");
                    return;
                }
            };

            loop {
                // Use timeout to allow periodic error recovery
                let message = tokio::time::timeout(
                    Duration::from_secs(5),
                    messages.next()
                ).await;

                match message {
                    Ok(Some(Ok(msg))) => {
                        let subject = msg.subject.as_str();

                        // Parse event based on subject
                        let event = if subject.starts_with("ergatai.file.ready.") {
                            match serde_json::from_slice::<FileReadyPayload>(&msg.payload) {
                                Ok(payload) => FileEvent::Ready(payload),
                                Err(e) => {
                                    warn!(error = %e, "Failed to parse file.ready payload");
                                    msg.ack().await.ok();
                                    continue;
                                }
                            }
                        } else if subject.starts_with("ergatai.file.error.") {
                            match serde_json::from_slice::<FileErrorPayload>(&msg.payload) {
                                Ok(payload) => FileEvent::Error(payload),
                                Err(e) => {
                                    warn!(error = %e, "Failed to parse file.error payload");
                                    msg.ack().await.ok();
                                    continue;
                                }
                            }
                        } else {
                            warn!(subject = subject, "Unknown file event subject");
                            msg.ack().await.ok();
                            continue;
                        };

                        // Acknowledge before processing
                        if let Err(e) = msg.ack().await {
                            warn!(error = %e, "Failed to ack file event");
                        }

                        match event {
                            FileEvent::Ready(payload) => {
                                info!(
                                    file_path = payload.file_path,
                                    agent_id = payload.agent_id,
                                    "File ready event received"
                                );
                                if let Err(e) = self.lock_manager.notify_file_ready(&payload.file_path).await {
                                    error!(
                                        file_path = payload.file_path,
                                        error = %e,
                                        "Failed to notify waiters"
                                    );
                                }
                            }
                            FileEvent::Error(payload) => {
                                warn!(
                                    file_path = payload.file_path,
                                    agent_id = payload.agent_id,
                                    reason = payload.reason,
                                    "File error event received"
                                );
                                if let Err(e) = self.lock_manager.notify_file_error(&payload.file_path, &payload.reason).await {
                                    error!(
                                        file_path = payload.file_path,
                                        error = %e,
                                        "Failed to notify waiters"
                                    );
                                }
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        warn!(error = %e, "Error receiving message");
                    }
                    Ok(None) => {
                        // Stream ended (consumer deleted or disconnected)
                        warn!("File events message stream ended unexpectedly");
                        break;
                    }
                    Err(_) => {
                        // Timeout — no message in 5s, loop continues (keeps stream alive)
                    }
                }
            }

            info!("File events consumer stopped");
        })
    }

    /// Consume the next file event from the queue
    ///
    /// Returns None if no events are available.
    /// The event must be acknowledged with `ack()` or it will be redelivered.
    pub async fn consume_next(&self) -> ErgataiResult<Option<FileEvent>> {
        let stream = self.connection.jetstream().get_stream(&self.stream_name).await
            .map_err(|e| ErgataiError::NatsError(format!("Stream {} not found: {}", self.stream_name, e)))?;

        // Create or get consumer
        let consumer_config = pull::Config {
            durable_name: Some(self.consumer_name.clone()),
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            ack_wait: Duration::from_secs(30),
            max_deliver: 3,
            ..Default::default()
        };

        let consumer: PullConsumer = stream.get_or_create_consumer(&self.consumer_name, consumer_config).await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to create consumer: {}", e)))?;

        // Fetch one message
        let mut messages = consumer.messages().await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to get messages: {}", e)))?;

        // Try to get a message with timeout
        let message = tokio::time::timeout(
            Duration::from_millis(100),
            messages.next()
        ).await;

        match message {
            Ok(Some(Ok(msg))) => {
                let subject = msg.subject.as_str();

                // Parse event based on subject
                let event = if subject.starts_with("ergatai.file.ready.") {
                    // File ready event
                    let payload: FileReadyPayload = serde_json::from_slice(&msg.payload)?;
                    FileEvent::Ready(payload)
                } else if subject.starts_with("ergatai.file.error.") {
                    // File error event
                    let payload: FileErrorPayload = serde_json::from_slice(&msg.payload)?;
                    FileEvent::Error(payload)
                } else {
                    warn!(subject = subject, "Unknown file event subject");
                    // Acknowledge unknown messages to remove them from queue
                    msg.ack().await.ok();
                    return Ok(None);
                };

                debug!(subject = subject, "File event consumed");
                // Acknowledge the message so it's removed from the queue
                if let Err(e) = msg.ack().await {
                    warn!(error = %e, "Failed to ack file event message");
                }
                Ok(Some(event))
            }
            Ok(Some(Err(e))) => {
                warn!(error = %e, "Error receiving message");
                Err(ErgataiError::NatsError(format!("Message receive error: {}", e)))
            }
            Ok(None) | Err(_) => {
                // No messages available or timeout
                Ok(None)
            }
        }
    }
}

/// File event types
#[derive(Debug)]
pub enum FileEvent {
    /// File ready event (WRITE completed)
    Ready(FileReadyPayload),
    /// File error event (writer crashed)
    Error(FileErrorPayload),
}

/// File ready payload
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileReadyPayload {
    /// File path
    pub file_path: String,
    /// Agent that completed the WRITE
    pub agent_id: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_ready_payload_serialization() {
        let payload = FileReadyPayload {
            file_path: "/path/to/file.txt".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_vec(&payload).unwrap();
        let deserialized: FileReadyPayload = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.file_path, payload.file_path);
        assert_eq!(deserialized.agent_id, payload.agent_id);
        assert_eq!(deserialized.timestamp, payload.timestamp);
    }

    #[test]
    fn test_file_error_payload_serialization() {
        let payload = FileErrorPayload {
            file_path: "/path/to/file.txt".to_string(),
            agent_id: "agent-1".to_string(),
            reason: "Agent crashed".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_vec(&payload).unwrap();
        let deserialized: FileErrorPayload = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.file_path, payload.file_path);
        assert_eq!(deserialized.agent_id, payload.agent_id);
        assert_eq!(deserialized.reason, payload.reason);
        assert_eq!(deserialized.timestamp, payload.timestamp);
    }
}
