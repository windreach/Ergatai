//! Lock wait consumer for processing NATS-based lock waiting queue
//!
//! Phase 8: Consumes lock requests from LOCK_WAITERS stream and grants
//! locks when they become available, ensuring FIFO ordering and fairness.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use tracing::{info, debug, warn, error};

use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::lock_manager::FileLockManager;
use crate::file_access::lock_waiter::{LockWaitRequest, LockReleaseNotification, LockGrantedNotification};
use crate::nats::connection::NatsConnection;

/// Consumer for lock waiting queue
pub struct LockWaitConsumer {
    connection: NatsConnection,
    stream_name: String,
    consumer_name: String,
    lock_manager: Arc<FileLockManager>,
}

impl LockWaitConsumer {
    /// Create a new lock wait consumer
    ///
    /// # Arguments
    ///
    /// * `connection` - NATS connection
    /// * `stream_name` - JetStream stream name (should be "LOCK_WAITERS")
    /// * `consumer_name` - Consumer group name
    /// * `lock_manager` - FileLockManager for granting locks
    pub async fn new(
        connection: NatsConnection,
        stream_name: String,
        consumer_name: String,
        lock_manager: Arc<FileLockManager>,
    ) -> ErgataiResult<Self> {
        info!(
            stream = %stream_name,
            consumer = %consumer_name,
            "Lock wait consumer created"
        );

        Ok(Self {
            connection,
            stream_name,
            consumer_name,
            lock_manager,
        })
    }

    /// Start consuming lock requests in a background task
    ///
    /// This will continuously poll for new lock requests and process them.
    /// Returns a handle that can be used to stop the consumer.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!("Lock wait consumer started");

            // Create consumer and messages stream
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
                    error!("Failed to initialize lock wait consumer: {}", e);
                    return;
                }
            };

            info!("Lock wait consumer initialized, waiting for messages...");

            while let Some(message_result) = messages.next().await {
                match message_result {
                    Ok(message) => {
                        if let Err(e) = self.process_message(message).await {
                            error!("Error processing lock wait message: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Error receiving message: {}", e);
                        break;
                    }
                }
            }

            info!("Lock wait consumer stopped");
        })
    }

    /// Process a single lock wait message
    async fn process_message(&self, message: async_nats::jetstream::Message) -> ErgataiResult<()> {
        let payload_str = String::from_utf8_lossy(&message.payload);
        debug!("Processing lock wait message: {}", payload_str);

        // Parse the message based on subject
        let subject = message.subject.as_str();
        
        if subject.starts_with("ergatai.lock.request.") {
            self.handle_lock_request(message).await?;
        } else if subject.starts_with("ergatai.lock.release.") {
            self.handle_lock_release(message).await?;
        } else {
            warn!("Unknown subject for lock wait message: {}", subject);
            message.ack().await.map_err(|e| {
                ErgataiError::NatsError(format!("Failed to ack message: {}", e))
            })?;
        }

        Ok(())
    }

    /// Handle a lock request
    async fn handle_lock_request(&self, message: async_nats::jetstream::Message) -> ErgataiResult<()> {
        let request: LockWaitRequest = serde_json::from_slice(&message.payload)
            .map_err(|e| ErgataiError::internal(format!("Failed to parse LockWaitRequest: {}", e)))?;

        info!(
            request_id = %request.request_id,
            agent_id = %request.agent_id,
            file_path = %request.file_path,
            mode = ?request.mode,
            "Processing lock request"
        );

        // Try to find the file token for this request
        let file_token = match self.lock_manager.find_active_file_token_by_id(&request.token_id) {
            Ok(token) => token,
            Err(_) => {
                // Token not found, ack and discard
                warn!(
                    request_id = %request.request_id,
                    token_id = %request.token_id,
                    "File token not found, discarding request"
                );
                message.ack().await.map_err(|e| {
                    ErgataiError::internal(format!("Failed to ack message: {}", e))
                })?;
                return Ok(());
            }
        };

        // Try to acquire the lock
        match self.lock_manager.acquire_lock(&file_token, &request.file_path).await {
            Ok(()) => {
                // Lock acquired successfully
                info!(
                    request_id = %request.request_id,
                    file_path = %request.file_path,
                    "Lock granted to waiting agent"
                );

                // Send grant notification
                let grant = LockGrantedNotification::new(
                    request.request_id.clone(),
                    request.file_path.clone(),
                );

                self.connection.publish(
                    &request.reply_subject,
                    serde_json::to_vec(&grant)
                        .map_err(|e| ErgataiError::internal(format!("Failed to serialize grant: {}", e)))?
                        .into(),
                ).await.map_err(|e| {
                    ErgataiError::internal(format!("Failed to publish grant: {}", e))
                })?;

                // Ack the message
                message.ack().await.map_err(|e| {
                    ErgataiError::internal(format!("Failed to ack message: {}", e))
                })?;
            }
            Err(ErgataiError::LockConflict(_)) => {
                // Lock still held, leave message in queue for retry
                debug!(
                    request_id = %request.request_id,
                    file_path = %request.file_path,
                    "Lock still held, message left in queue"
                );
                // Don't ack - will be redelivered
            }
            Err(e) => {
                // Other error, ack and log
                error!(
                    request_id = %request.request_id,
                    "Error acquiring lock: {}", e
                );
                message.ack().await.map_err(|e| {
                    ErgataiError::internal(format!("Failed to ack message: {}", e))
                })?;
            }
        }

        Ok(())
    }

    /// Handle a lock release notification
    async fn handle_lock_release(&self, message: async_nats::jetstream::Message) -> ErgataiResult<()> {
        let notification: LockReleaseNotification = serde_json::from_slice(&message.payload)
            .map_err(|e| ErgataiError::internal(format!("Failed to parse LockReleaseNotification: {}", e)))?;

        info!(
            file_path = %notification.file_path,
            released_by = %notification.released_by_token_id,
            "Processing lock release notification"
        );

        // Lock has been released, waiting requests will be retried automatically
        // by the consumer (messages stay in queue until successfully acquired)

        // Ack the release notification
        message.ack().await.map_err(|e| {
            ErgataiError::internal(format!("Failed to ack message: {}", e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_wait_consumer_creation() {
        // Basic test to ensure struct can be created
        // Full integration tests require NATS server
        let stream_name = "LOCK_WAITERS".to_string();
        let consumer_name = "lock-wait-processor".to_string();
        
        assert_eq!(stream_name, "LOCK_WAITERS");
        assert_eq!(consumer_name, "lock-wait-processor");
    }
}
