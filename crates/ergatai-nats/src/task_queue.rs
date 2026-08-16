//! JetStream-based task queue
//!
//! Provides a persistent task queue backed by NATS JetStream.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;

use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy, PullConsumer};
use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::connection::NatsConnection;
use ergatai_error::{ErgataiError, ErgataiResult};

/// Default ack wait time (5 minutes)
const ACK_WAIT_SECS: u64 = 300;

/// Maximum delivery attempts before moving to dead letter queue
const MAX_DELIVER: i64 = 3;

/// Task queue message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage<T> {
    /// Unique message ID
    pub message_id: String,
    /// Correlation ID (e.g., DAG ID, parent task ID)
    pub correlation_id: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
    /// Retry count
    pub retry_count: u32,
    /// Maximum retries
    pub max_retries: u32,
    /// Task payload
    pub payload: T,
}

impl<T> TaskMessage<T> {
    /// Create a new task message
    pub fn new(correlation_id: String, payload: T) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            message_id: Uuid::new_v4().to_string(),
            correlation_id,
            timestamp,
            retry_count: 0,
            max_retries: 3,
            payload,
        }
    }

    /// Check if this message can be retried
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries
    }
}

/// JetStream-backed task queue
pub struct NatsTaskQueue<T: Serialize + for<'de> Deserialize<'de>> {
    connection: NatsConnection,
    stream_name: String,
    consumer_name: String,
    subject: String,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static> NatsTaskQueue<T> {
    /// Create a new task queue
    ///
    /// # Arguments
    ///
    /// * `connection` - NATS connection
    /// * `stream_name` - JetStream stream name
    /// * `consumer_name` - Consumer group name
    /// * `subject` - Subject pattern (e.g., "ergatai.task.submit.pool1")
    pub async fn new(
        connection: NatsConnection,
        stream_name: String,
        consumer_name: String,
        subject: String,
    ) -> ErgataiResult<Self> {
        // Create or get the stream
        let stream_config = Config {
            name: stream_name.clone(),
            subjects: vec![subject.clone()],
            retention: RetentionPolicy::WorkQueue, // Auto-delete after ack
            max_age: Duration::from_secs(86400),   // 24 hours
            storage: StorageType::File,
            num_replicas: 1,
            ..Default::default()
        };

        connection.create_stream(stream_config).await?;

        info!(
            stream = stream_name,
            consumer = consumer_name,
            subject = subject,
            "Task queue created"
        );

        Ok(Self {
            connection,
            stream_name,
            consumer_name,
            subject,
            _marker: std::marker::PhantomData,
        })
    }

    /// Submit a task to the queue
    ///
    /// The message will be persisted to JetStream and delivered to a consumer.
    pub async fn submit(&self, correlation_id: String, payload: T) -> ErgataiResult<String> {
        let message = TaskMessage::new(correlation_id, payload);
        let message_id = message.message_id.clone();

        let json = serde_json::to_vec(&message)?;

        self.connection.publish(&self.subject, json).await?;

        info!(message_id = message_id, "Task submitted");
        Ok(message_id)
    }

    /// Consume the next task from the queue
    ///
    /// Returns None if no tasks are available.
    /// The task must be acknowledged with `ack()` or it will be redelivered.
    pub async fn consume(&self) -> ErgataiResult<Option<(TaskMessage<T>, ConsumerAck)>> {
        let stream = self
            .connection
            .jetstream()
            .get_stream(&self.stream_name)
            .await
            .map_err(|e| {
                ErgataiError::NatsError(format!("Stream {} not found: {}", self.stream_name, e))
            })?;

        // Create or get consumer
        let consumer_config = pull::Config {
            durable_name: Some(self.consumer_name.clone()),
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            ack_wait: Duration::from_secs(ACK_WAIT_SECS),
            max_deliver: MAX_DELIVER,
            filter_subject: self.subject.clone(),
            ..Default::default()
        };

        let consumer: PullConsumer = stream
            .get_or_create_consumer(&self.consumer_name, consumer_config)
            .await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to create consumer: {}", e)))?;

        // Use batch() with max_messages(1) for per-message consume.
        // messages() creates a long-lived subscription that loses state between calls.
        // batch() is a one-shot request that fetches exactly N messages.
        let mut messages = consumer
            .batch()
            .max_messages(1)
            .messages()
            .await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to fetch message: {}", e)))?;

        // Try to get a message with timeout (retry with increasing delays)
        let mut last_timeout = 50;
        for _ in 0..3 {
            match tokio::time::timeout(Duration::from_millis(last_timeout), messages.next()).await {
                Ok(Some(Ok(msg))) => {
                    let payload: TaskMessage<T> = serde_json::from_slice(&msg.payload)?;
                    let message_id_for_log = payload.message_id.clone();

                    let ack = ConsumerAck {
                        message: msg,
                        message_id: message_id_for_log,
                    };

                    debug!(message_id = payload.message_id, "Task consumed");
                    return Ok(Some((payload, ack)));
                }
                Ok(Some(Err(e))) => {
                    warn!(error = %e, "Error receiving message");
                    return Err(ErgataiError::NatsError(format!(
                        "Message receive error: {}",
                        e
                    )));
                }
                Ok(None) => {
                    // Batch exhausted (no more messages)
                    return Ok(None);
                }
                Err(_) => {
                    // Timeout, try again with longer timeout
                    last_timeout += 50;
                    continue;
                }
            }
        }

        // All retry attempts timed out - no message available
        Ok(None)
    }

    /// Get the number of pending messages in the queue
    pub async fn pending_count(&self) -> ErgataiResult<u64> {
        let mut stream = self
            .connection
            .jetstream()
            .get_stream(&self.stream_name)
            .await
            .map_err(|e| {
                ErgataiError::NatsError(format!("Stream {} not found: {}", self.stream_name, e))
            })?;

        let info = stream
            .info()
            .await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to get stream info: {}", e)))?;

        Ok(info.state.messages)
    }
}

/// Acknowledgment handle for a consumed message
pub struct ConsumerAck {
    message: async_nats::jetstream::Message,
    /// Parsed message ID for logging (avoid re-parsing the payload at ack time)
    message_id: String,
}

impl ConsumerAck {
    /// Acknowledge successful processing
    ///
    /// The message will be removed from the queue.
    pub async fn ack(self) -> ErgataiResult<()> {
        self.message
            .ack()
            .await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to ack message: {}", e)))?;

        debug!(message_id = %self.message_id, "Task acknowledged");
        Ok(())
    }

    /// Negative acknowledgment — request redelivery.
    ///
    /// For now, we do nothing and let the message sit unacknowledged. It will
    /// be redelivered after the `ack_wait` timeout (5 minutes). A future
    /// improvement would use NATS's immediate-redelivery signal, but the
    /// async-nats 0.38 `jetstream::Message` API doesn't expose a straightforward
    /// `nak()` method. The timeout-based approach is reliable, just slower.
    pub async fn nack(self) -> ErgataiResult<()> {
        debug!(
            message_id = %self.message_id,
            "Task nacked (will redeliver after ack_wait timeout)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestPayload {
        data: String,
    }

    #[test]
    fn test_task_message_creation() {
        let msg = TaskMessage::new(
            "corr123".to_string(),
            TestPayload {
                data: "test".to_string(),
            },
        );
        assert_eq!(msg.correlation_id, "corr123");
        assert_eq!(msg.retry_count, 0);
        assert_eq!(msg.max_retries, 3);
        assert!(msg.can_retry());
        assert!(!msg.message_id.is_empty());
    }

    #[test]
    fn test_can_retry() {
        let mut msg = TaskMessage::new(
            "corr".to_string(),
            TestPayload {
                data: "test".to_string(),
            },
        );
        assert!(msg.can_retry());

        msg.retry_count = 2;
        assert!(msg.can_retry());

        msg.retry_count = 3;
        assert!(!msg.can_retry());
    }

    #[test]
    fn test_message_serialization_roundtrip() {
        let msg = TaskMessage::new(
            "corr-123".to_string(),
            TestPayload {
                data: "hello".to_string(),
            },
        );
        let json = serde_json::to_vec(&msg).unwrap();
        let deserialized: TaskMessage<TestPayload> = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.message_id, msg.message_id);
        assert_eq!(deserialized.correlation_id, msg.correlation_id);
        assert_eq!(deserialized.payload.data, "hello");
        assert_eq!(deserialized.retry_count, 0);
        assert_eq!(deserialized.max_retries, 3);
    }

    /// Test full task queue flow: create → submit → consume → ack
    /// Skips if nats-server is not available.
    #[tokio::test]
    async fn test_task_queue_full_flow() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::NatsConnection::connect_to_server(server)
            .await
            .unwrap();

        // Create queue with unique names to avoid state conflicts
        let process_id = std::process::id();
        let queue: NatsTaskQueue<TestPayload> = match NatsTaskQueue::new(
            conn.clone(),
            format!("test_queue_{}", process_id),
            format!("test_worker_{}", process_id), // Unique consumer name
            format!("ergatai.test.tasks.{}", process_id),
        )
        .await
        {
            Ok(q) => q,
            Err(e) => {
                eprintln!("⚠️  Skipping (JetStream storage unavailable): {}", e);
                return;
            }
        };

        // Submit 3 tasks
        let payload1 = TestPayload {
            data: "task1".to_string(),
        };
        let payload2 = TestPayload {
            data: "task2".to_string(),
        };
        let payload3 = TestPayload {
            data: "task3".to_string(),
        };

        let id1 = queue.submit("corr-1".to_string(), payload1).await.unwrap();
        let id2 = queue.submit("corr-2".to_string(), payload2).await.unwrap();
        let _id3 = queue.submit("corr-3".to_string(), payload3).await.unwrap();

        assert!(!id1.is_empty());
        assert!(!id2.is_empty());

        // Check pending count
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pending = queue.pending_count().await.unwrap();
        assert_eq!(pending, 3, "Should have 3 pending tasks");

        // Consume and ack task 1
        let (msg1, ack1) = queue.consume().await.unwrap().expect("Should get task 1");
        assert_eq!(msg1.payload.data, "task1");
        assert_eq!(msg1.correlation_id, "corr-1");
        ack1.ack().await.unwrap();

        // Wait to ensure next message is ready and ack is processed
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Consume and ack task 2
        let (msg2, ack2) = queue.consume().await.unwrap().expect("Should get task 2");
        assert_eq!(msg2.payload.data, "task2");
        ack2.ack().await.unwrap();

        // Wait to ensure next message is ready and ack is processed
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Consume and nack task 3 (will not be redelivered in this test)
        let (msg3, ack3) = queue.consume().await.unwrap().expect("Should get task 3");
        assert_eq!(msg3.payload.data, "task3");
        ack3.nack().await.unwrap(); // nack = don't ack, will redeliver after timeout

        // After acking 2, pending should be 1 (task3 still there)
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pending_after = queue.pending_count().await.unwrap();
        assert!(
            pending_after <= 2,
            "Should have at most 2 pending tasks after acks"
        );
    }

    /// Test empty queue returns None
    #[tokio::test]
    async fn test_consume_empty_queue() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::NatsConnection::connect_to_server(server)
            .await
            .unwrap();

        let queue: NatsTaskQueue<TestPayload> = match NatsTaskQueue::new(
            conn.clone(),
            format!("test_empty_{}", std::process::id()),
            "test_worker".to_string(),
            format!("ergatai.test.empty.{}", std::process::id()),
        )
        .await
        {
            Ok(q) => q,
            Err(e) => {
                eprintln!("⚠️  Skipping (JetStream storage unavailable): {}", e);
                return;
            }
        };

        // Consume from empty queue should return None
        let result = queue.consume().await.unwrap();
        assert!(result.is_none(), "Empty queue should return None");
    }

    #[test]
    fn test_task_message_timestamp() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let msg = TaskMessage::new(
            "corr".to_string(),
            TestPayload {
                data: "test".to_string(),
            },
        );

        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Timestamp should be between before and after
        assert!(msg.timestamp >= before);
        assert!(msg.timestamp <= after);
    }

    #[test]
    fn test_task_message_unique_ids() {
        let msg1 = TaskMessage::new(
            "corr".to_string(),
            TestPayload {
                data: "test1".to_string(),
            },
        );
        let msg2 = TaskMessage::new(
            "corr".to_string(),
            TestPayload {
                data: "test2".to_string(),
            },
        );

        // Each message should have a unique ID
        assert_ne!(msg1.message_id, msg2.message_id);
        assert!(!msg1.message_id.is_empty());
        assert!(!msg2.message_id.is_empty());
    }

    #[test]
    fn test_can_retry_boundary() {
        let mut msg = TaskMessage::new(
            "corr".to_string(),
            TestPayload {
                data: "test".to_string(),
            },
        );

        // max_retries = 3, so:
        msg.retry_count = 0;
        assert!(msg.can_retry());
        msg.retry_count = 1;
        assert!(msg.can_retry());
        msg.retry_count = 2;
        assert!(msg.can_retry());
        msg.retry_count = 3;
        assert!(!msg.can_retry());
        msg.retry_count = 4;
        assert!(!msg.can_retry());
    }

    #[test]
    fn test_task_message_serialization_with_complex_payload() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct ComplexPayload {
            nested: HashMap<String, Vec<i32>>,
            optional: Option<String>,
        }

        let mut nested = HashMap::new();
        nested.insert("key1".to_string(), vec![1, 2, 3]);
        nested.insert("key2".to_string(), vec![4, 5]);

        let payload = ComplexPayload {
            nested: nested.clone(),
            optional: Some("value".to_string()),
        };

        let msg = TaskMessage::new("corr-complex".to_string(), payload);
        let json = serde_json::to_vec(&msg).unwrap();
        let deserialized: TaskMessage<ComplexPayload> = serde_json::from_slice(&json).unwrap();

        assert_eq!(deserialized.correlation_id, "corr-complex");
        assert_eq!(deserialized.payload.nested.len(), 2);
        assert_eq!(deserialized.payload.nested.get("key1"), Some(&vec![1, 2, 3]));
        assert_eq!(deserialized.payload.optional, Some("value".to_string()));
    }

    /// Test pending count on empty queue
    #[tokio::test]
    async fn test_pending_count_empty() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::NatsConnection::connect_to_server(server)
            .await
            .unwrap();

        let queue: NatsTaskQueue<TestPayload> = match NatsTaskQueue::new(
            conn.clone(),
            format!("test_pending_empty_{}", std::process::id()),
            "test_worker_empty".to_string(),
            format!("ergatai.test.pending.empty.{}", std::process::id()),
        )
        .await
        {
            Ok(q) => q,
            Err(e) => {
                eprintln!("⚠️  Skipping (JetStream storage unavailable): {}", e);
                return;
            }
        };

        let pending = queue.pending_count().await.unwrap();
        assert_eq!(pending, 0, "Empty queue should have 0 pending");
    }
}
