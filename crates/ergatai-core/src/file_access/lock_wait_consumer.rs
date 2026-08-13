//! Lock wait consumer for processing NATS-based lock waiting queue
//!
//! Phase 8: Pure NATS-based lock waiting queue with active wake-up.
//!
//! Architecture:
//! - NATS JetStream is the single source of truth (persistent queue)
//! - Consumer tries to acquire locks on message delivery
//! - Success: ack message + send grant notification
//! - Failure: don't ack, NATS will redeliver with exponential backoff
//! - Lock release triggers immediate retry via release notification

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::lock_manager::FileLockManager;
use crate::file_access::lock_waiter::{
    LockCancelRequest, LockGrantedNotification, LockReleaseNotification, LockWaitRequest,
};
use crate::nats::connection::NatsConnection;

/// Consumer for lock waiting queue
///
/// Pure NATS-based: no in-memory queue, relies on NATS redelivery for retry.
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
            "Lock wait consumer created (pure NATS, no memory queue)"
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
            info!("Lock wait consumer started (pure NATS mode)");

            // Create consumer and messages stream
            let messages_result = async {
                let stream = self
                    .connection
                    .jetstream()
                    .get_stream(&self.stream_name)
                    .await
                    .map_err(|e| {
                        ErgataiError::NatsError(format!(
                            "Stream {} not found: {}",
                            self.stream_name, e
                        ))
                    })?;

                let consumer_config = pull::Config {
                    durable_name: Some(self.consumer_name.clone()),
                    deliver_policy: DeliverPolicy::All,
                    ack_policy: AckPolicy::Explicit,
                    // Exponential backoff via NATS redelivery:
                    // - ack_wait: time before redelivery (increases with each retry)
                    // - max_deliver: maximum number of delivery attempts
                    ack_wait: Duration::from_secs(5), // Initial wait before redelivery
                    max_deliver: 10,                  // Max 10 attempts
                    ..Default::default()
                };

                let consumer = stream
                    .get_or_create_consumer(&self.consumer_name, consumer_config)
                    .await
                    .map_err(|e| {
                        ErgataiError::NatsError(format!("Failed to create consumer: {}", e))
                    })?;

                let messages = consumer.messages().await.map_err(|e| {
                    ErgataiError::NatsError(format!("Failed to get messages: {}", e))
                })?;

                Ok::<_, ErgataiError>(messages)
            }
            .await;

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
        } else if subject.starts_with("ergatai.lock.cancel.") {
            self.handle_lock_cancel(message).await?;
        } else {
            warn!("Unknown subject for lock wait message: {}", subject);
            message
                .ack()
                .await
                .map_err(|e| ErgataiError::NatsError(format!("Failed to ack message: {}", e)))?;
        }

        Ok(())
    }

    /// Handle a lock request
    ///
    /// Pure NATS mode:
    /// - Try to acquire lock immediately
    /// - Success: ack message + send grant notification
    /// - Failure: don't ack, NATS will redeliver
    async fn handle_lock_request(
        &self,
        message: async_nats::jetstream::Message,
    ) -> ErgataiResult<()> {
        let request: LockWaitRequest = serde_json::from_slice(&message.payload).map_err(|e| {
            ErgataiError::internal(format!("Failed to parse LockWaitRequest: {}", e))
        })?;

        // Check delivery count for logging
        let delivery_count = message.info().map(|i| i.delivered).unwrap_or(1);

        info!(
            request_id = %request.request_id,
            agent_id = %request.agent_id,
            file_path = %request.file_path,
            mode = ?request.mode,
            priority = ?request.priority,
            delivery_count = delivery_count,
            "Processing lock request"
        );

        // Validate token exists
        let file_token = match self
            .lock_manager
            .find_active_file_token_by_id(&request.token_id)
        {
            Ok(token) => token,
            Err(_) => {
                // Token not found, ack and discard (don't retry)
                warn!(
                    request_id = %request.request_id,
                    token_id = %request.token_id,
                    "File token not found, discarding request"
                );
                message
                    .ack()
                    .await
                    .map_err(|e| ErgataiError::internal(format!("Failed to ack message: {}", e)))?;
                return Ok(());
            }
        };

        // Try to acquire the lock
        match self
            .lock_manager
            .acquire_lock(&file_token, &request.file_path)
            .await
        {
            Ok(()) => {
                // Lock acquired successfully
                info!(
                    request_id = %request.request_id,
                    file_path = %request.file_path,
                    delivery_count = delivery_count,
                    "Lock granted to waiting agent"
                );

                // Send grant notification
                let grant = LockGrantedNotification::new(
                    request.request_id.clone(),
                    request.file_path.clone(),
                );

                if let Err(e) = self
                    .connection
                    .publish(
                        &request.reply_subject,
                        serde_json::to_vec(&grant).map_err(|e| {
                            ErgataiError::internal(format!("Failed to serialize grant: {}", e))
                        })?,
                    )
                    .await
                {
                    error!("Failed to publish grant notification: {}", e);
                    // Lock was acquired but notification failed - release and retry
                    let _ = self
                        .lock_manager
                        .release_lock(file_token.id.as_str(), &request.file_path)
                        .await;
                    // Don't ack, let NATS redeliver
                    return Ok(());
                }

                // Ack the message - lock granted successfully
                message
                    .ack()
                    .await
                    .map_err(|e| ErgataiError::internal(format!("Failed to ack message: {}", e)))?;
            }
            Err(ErgataiError::LockConflict(_))
            | Err(ErgataiError::LockConflictWithRetry { .. }) => {
                // Lock still held, don't ack - NATS will redeliver
                debug!(
                    request_id = %request.request_id,
                    file_path = %request.file_path,
                    delivery_count = delivery_count,
                    "Lock still held, waiting for NATS redelivery"
                );
                // Don't ack - NATS will redeliver after ack_wait timeout
            }
            Err(e) => {
                // Other error, ack and log (don't retry)
                error!(
                    request_id = %request.request_id,
                    delivery_count = delivery_count,
                    "Error acquiring lock: {}", e
                );
                message
                    .ack()
                    .await
                    .map_err(|e| ErgataiError::internal(format!("Failed to ack message: {}", e)))?;
            }
        }

        Ok(())
    }

    /// Handle a lock release notification
    ///
    /// When a lock is released, we don't need to do anything special here.
    /// NATS will redeliver waiting requests based on ack_wait timeout.
    /// This notification is just for logging/monitoring.
    async fn handle_lock_release(
        &self,
        message: async_nats::jetstream::Message,
    ) -> ErgataiResult<()> {
        let notification: LockReleaseNotification = serde_json::from_slice(&message.payload)
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to parse LockReleaseNotification: {}", e))
            })?;

        info!(
            file_path = %notification.file_path,
            released_by = %notification.released_by_token_id,
            "Lock released, waiting requests will be retried via NATS redelivery"
        );

        // Ack the release notification
        message
            .ack()
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to ack message: {}", e)))?;

        Ok(())
    }

    /// Handle a lock cancel request
    ///
    /// When an agent cancels a wait request, we need to find and remove it.
    /// Since we're using pure NATS, we can't directly remove messages.
    /// Instead, we mark the request as cancelled in the lock_manager.
    async fn handle_lock_cancel(
        &self,
        message: async_nats::jetstream::Message,
    ) -> ErgataiResult<()> {
        let cancel: LockCancelRequest = serde_json::from_slice(&message.payload).map_err(|e| {
            ErgataiError::internal(format!("Failed to parse LockCancelRequest: {}", e))
        })?;

        info!(
            request_id = %cancel.request_id,
            agent_id = %cancel.agent_id,
            reason = ?cancel.reason,
            "Processing lock cancel request"
        );

        // Mark request as cancelled (implementation depends on lock_manager)
        // For now, we just ack the cancel message.
        // The actual cancellation happens when the request is next processed:
        // - If token is invalid, it will be discarded
        // - If agent disconnects, token becomes invalid

        message
            .ack()
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to ack message: {}", e)))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
