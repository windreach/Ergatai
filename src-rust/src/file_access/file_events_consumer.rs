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
use crate::nats::events::{FileErrorPayload, FileReadyPayload};

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
                        let parsed = parse_file_event(subject, &msg.payload);
                        let event = match parsed {
                            Ok(Some(e)) => e,
                            Ok(None) => {
                                warn!(subject = subject, "Unknown file event subject");
                                msg.ack().await.ok();
                                continue;
                            }
                            Err(e) => {
                                warn!(error = %e, subject = subject, "Failed to parse file event payload");
                                msg.ack().await.ok();
                                continue;
                            }
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
    /// Messages are acknowledged internally before returning — the caller does not
    /// need to call `ack()` separately. Malformed payloads are ACKed before returning
    /// Err because they cannot succeed on retry.
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
                let parsed = parse_file_event(subject, &msg.payload);
                let event = match parsed {
                    Ok(Some(e)) => e,
                    Ok(None) => {
                        warn!(subject = subject, "Unknown file event subject");
                        // Acknowledge unknown messages to remove them from queue
                        msg.ack().await.ok();
                        return Ok(None);
                    }
                    Err(e) => {
                        warn!(error = %e, subject = subject, "Failed to parse file event payload");
                        // ACK before returning the error: malformed JSON can never succeed on
                        // retry, so we remove it from the queue to avoid wasting redelivery
                        // attempts (consistent with `start()` behavior).
                        msg.ack().await.ok();
                        return Err(e);
                    }
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

/// Parse a NATS subject + payload into a FileEvent.
///
/// Returns:
/// - `Ok(Some(event))` if the subject matches `ergatai.file.ready.*` or `ergatai.file.error.*`
///   and the payload deserializes correctly.
/// - `Ok(None)` if the subject doesn't match either prefix (unknown subject).
/// - `Err(...)` if the subject matches but the payload is malformed JSON.
///
/// Pure function — no NATS/async dependency, easy to unit-test.
fn parse_file_event(subject: &str, payload: &[u8]) -> Result<Option<FileEvent>, ErgataiError> {
    if subject.starts_with("ergatai.file.ready.") {
        let p: FileReadyPayload = serde_json::from_slice(payload)?;
        Ok(Some(FileEvent::Ready(p)))
    } else if subject.starts_with("ergatai.file.error.") {
        let p: FileErrorPayload = serde_json::from_slice(payload)?;
        Ok(Some(FileEvent::Error(p)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Payload serialization (original tests) ────────────────────

    #[test]
    fn test_file_ready_payload_serialization() {
        let payload = FileReadyPayload {
            file_path: "/path/to/file.txt".to_string(),
            agent_id: "agent-1".to_string(),
            token_id: "token-abc".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_vec(&payload).unwrap();
        let deserialized: FileReadyPayload = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.file_path, payload.file_path);
        assert_eq!(deserialized.agent_id, payload.agent_id);
        assert_eq!(deserialized.token_id, payload.token_id);
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

    // ─── parse_file_event: happy paths ─────────────────────────────

    #[test]
    fn test_parse_file_event_ready() {
        let payload = FileReadyPayload {
            file_path: "src/main.rs".to_string(),
            agent_id: "agent-42".to_string(),
            token_id: "token-42".to_string(),
            timestamp: 1_700_000_000,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();

        let result = parse_file_event("ergatai.file.ready.abc123", &bytes).unwrap();
        match result {
            Some(FileEvent::Ready(p)) => {
                assert_eq!(p.file_path, "src/main.rs");
                assert_eq!(p.agent_id, "agent-42");
                assert_eq!(p.token_id, "token-42");
                assert_eq!(p.timestamp, 1_700_000_000);
            }
            other => panic!("expected Some(FileEvent::Ready(..)), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_file_event_error() {
        let payload = FileErrorPayload {
            file_path: "src/lib.rs".to_string(),
            agent_id: "agent-7".to_string(),
            reason: "OOM killed".to_string(),
            timestamp: 1_700_000_001,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();

        let result = parse_file_event("ergatai.file.error.def456", &bytes).unwrap();
        match result {
            Some(FileEvent::Error(p)) => {
                assert_eq!(p.file_path, "src/lib.rs");
                assert_eq!(p.agent_id, "agent-7");
                assert_eq!(p.reason, "OOM killed");
            }
            other => panic!("expected Some(FileEvent::Error(..)), got {:?}", other),
        }
    }

    // ─── parse_file_event: unknown subject ─────────────────────────

    #[test]
    fn test_parse_file_event_unknown_subject_returns_none() {
        let payload = serde_json::to_vec(&FileReadyPayload {
            file_path: "x".to_string(),
            agent_id: "a".to_string(),
            token_id: "t".to_string(),
            timestamp: 0,
        })
        .unwrap();

        // Subject doesn't match either prefix → None, not an error
        let result = parse_file_event("ergatai.unknown.subject", &payload).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_file_event_similar_but_wrong_prefix() {
        let payload = serde_json::to_vec(&FileReadyPayload {
            file_path: "x".to_string(),
            agent_id: "a".to_string(),
            token_id: "t".to_string(),
            timestamp: 0,
        })
        .unwrap();

        // Close but not matching — no dot after "ready"/"error"
        assert!(parse_file_event("ergatai.file.readyfoo", &payload)
            .unwrap()
            .is_none());
        assert!(parse_file_event("ergatai.file.errorfoo", &payload)
            .unwrap()
            .is_none());
        assert!(parse_file_event("ergatai.file", &payload).unwrap().is_none());
        assert!(parse_file_event("ergatai.", &payload).unwrap().is_none());
        assert!(parse_file_event("", &payload).unwrap().is_none());
    }

    // ─── parse_file_event: malformed payloads ──────────────────────

    #[test]
    fn test_parse_file_event_ready_invalid_json() {
        let bad = b"{\"file_path\": 42}"; // file_path should be a string
        let result = parse_file_event("ergatai.file.ready.abc", bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_file_event_ready_missing_field() {
        let bad = b"{\"file_path\": \"x\"}"; // missing agent_id and timestamp
        let result = parse_file_event("ergatai.file.ready.abc", bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_file_event_error_invalid_json() {
        let bad = b"not json at all";
        let result = parse_file_event("ergatai.file.error.xyz", bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_file_event_empty_payload() {
        let result = parse_file_event("ergatai.file.ready.abc", b"");
        assert!(result.is_err());
    }

    // ─── parse_file_event: edge-case values ────────────────────────

    #[test]
    fn test_parse_file_event_unicode_path() {
        let payload = FileReadyPayload {
            file_path: "src/路径/файл.rs".to_string(),
            agent_id: "agent-中文".to_string(),
            token_id: "token-unicode".to_string(),
            timestamp: 0,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();
        let result = parse_file_event("ergatai.file.ready.hash", &bytes).unwrap();
        match result {
            Some(FileEvent::Ready(p)) => {
                assert_eq!(p.file_path, "src/路径/файл.rs");
                assert_eq!(p.agent_id, "agent-中文");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn test_parse_file_event_empty_strings() {
        let payload = FileReadyPayload {
            file_path: "".to_string(),
            agent_id: "".to_string(),
            token_id: "".to_string(),
            timestamp: 0,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();
        let result = parse_file_event("ergatai.file.ready.hash", &bytes).unwrap();
        assert!(matches!(result, Some(FileEvent::Ready(_))));
    }

    #[test]
    fn test_parse_file_event_max_timestamp() {
        let payload = FileReadyPayload {
            file_path: "x".to_string(),
            agent_id: "a".to_string(),
            token_id: "t".to_string(),
            timestamp: u64::MAX,
        };
        let bytes = serde_json::to_vec(&payload).unwrap();
        let result = parse_file_event("ergatai.file.ready.h", &bytes).unwrap();
        match result {
            Some(FileEvent::Ready(p)) => assert_eq!(p.timestamp, u64::MAX),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn test_parse_file_event_extra_fields_ignored() {
        // JSON with extra fields should still deserialize (serde default)
        let bytes = br#"{"file_path":"f.rs","agent_id":"a","token_id":"t","timestamp":1,"extra":"ignored"}"#;
        let result = parse_file_event("ergatai.file.ready.h", bytes).unwrap();
        assert!(matches!(result, Some(FileEvent::Ready(_))));
    }

    // ─── FileEvent enum ────────────────────────────────────────────

    #[test]
    fn test_file_event_debug_formatting() {
        let ready = FileEvent::Ready(FileReadyPayload {
            file_path: "a.rs".to_string(),
            agent_id: "ag".to_string(),
            token_id: "tk".to_string(),
            timestamp: 1,
        });
        let debug_str = format!("{:?}", ready);
        assert!(debug_str.contains("Ready"));
        assert!(debug_str.contains("a.rs"));

        let error = FileEvent::Error(FileErrorPayload {
            file_path: "b.rs".to_string(),
            agent_id: "ag".to_string(),
            reason: "boom".to_string(),
            timestamp: 2,
        });
        let debug_str = format!("{:?}", error);
        assert!(debug_str.contains("Error"));
        assert!(debug_str.contains("boom"));
    }
}
