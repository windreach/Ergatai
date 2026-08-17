//! NATS connection wrapper
//!
//! Provides a thin wrapper around async-nats Client with Ergatai-specific configuration.

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::{self, Config};
use async_nats::Client;
use tracing::{debug, info};

use crate::server::NatsServer;
use ergatai_error::{ErgataiError, ErgataiResult};

/// NATS connection wrapper
///
/// Holds the async-nats Client and JetStream context.
/// Clone is cheap (Arc internally).
#[derive(Clone)]
pub struct NatsConnection {
    client: Client,
    jetstream: jetstream::Context,
}

impl NatsConnection {
    /// Connect to a NATS server
    ///
    /// # Arguments
    ///
    /// * `url` - NATS server URL (e.g., "127.0.0.1:4222")
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails.
    pub async fn connect(url: &str) -> ErgataiResult<Self> {
        info!(url = url, "Connecting to NATS");

        let client = async_nats::connect(url).await.map_err(|e| {
            ErgataiError::NatsError(format!("Failed to connect to NATS at {}: {}", url, e))
        })?;

        let jetstream = jetstream::new(client.clone());

        info!("Connected to NATS");

        Ok(Self { client, jetstream })
    }

    /// Connect to an embedded NatsServer instance
    pub async fn connect_to_server(server: &NatsServer) -> ErgataiResult<Self> {
        Self::connect(&server.url()).await
    }

    /// Get the underlying async-nats Client
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get the JetStream context
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.jetstream
    }

    /// Publish a message to a subject (core NATS — no persistence, no ack)
    ///
    /// Fire-and-forget semantics. For reliable delivery with persistence and
    /// delivery confirmation, use [`publish_jetstream`](Self::publish_jetstream) instead.
    pub async fn publish(&self, subject: &str, payload: Vec<u8>) -> ErgataiResult<()> {
        self.client
            .publish(subject.to_string(), payload.into())
            .await
            .map_err(|e| {
                ErgataiError::NatsError(format!("Failed to publish to {}: {}", subject, e))
            })?;

        debug!(subject = subject, "Published message");
        Ok(())
    }

    /// Publish a message to a JetStream-backed subject (persisted, ack-backed)
    ///
    /// The message is only considered "sent" when the JetStream stream has
    /// durably stored it and returned a `PublishAck`. If the target stream is
    /// unavailable, this returns an error — callers can then fall back to core
    /// NATS publish or surface the error to the user.
    ///
    /// # Reliability semantics
    ///
    /// - Returns `Ok(PublishAck)` once the stream has persisted the message
    /// - The ack contains `stream`, `sequence`, and `domain` for traceability
    /// - On stream unavailability, returns `Err` (caller decides fallback)
    pub async fn publish_jetstream(
        &self,
        subject: &str,
        payload: Vec<u8>,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        // Two-step await: jetstream.publish() returns a PublishAckFuture,
        // which must be awaited again to get the actual PublishAck from the stream.
        let ack_future = self
            .jetstream
            .publish(subject.to_string(), payload.into())
            .await
            .map_err(|e| {
                ErgataiError::NatsError(format!("JetStream publish to {} failed: {}", subject, e))
            })?;
        // Timeout the ack await — a stalled NATS server (network partition, slow disk,
        // leader election) should not block the caller indefinitely.
        let ack = tokio::time::timeout(Duration::from_secs(10), ack_future)
            .await
            .map_err(|_| {
                ErgataiError::NatsError(format!(
                    "JetStream ack for {} timed out after 10s",
                    subject
                ))
            })?
            .map_err(|e| {
                ErgataiError::NatsError(format!("JetStream ack for {} failed: {}", subject, e))
            })?;

        debug!(
            subject = subject,
            stream = ack.stream.as_str(),
            sequence = ack.sequence,
            "JetStream message persisted"
        );
        Ok(ack)
    }

    /// Subscribe to a subject
    pub async fn subscribe(&self, subject: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subscriber = self
            .client
            .subscribe(subject.to_string())
            .await
            .map_err(|e| {
                ErgataiError::NatsError(format!("Failed to subscribe to {}: {}", subject, e))
            })?;

        debug!(subject = subject, "Subscribed");
        Ok(subscriber)
    }

    /// Create or get a JetStream stream
    pub async fn create_stream(&self, config: Config) -> ErgataiResult<stream::Stream> {
        let mut stream = self
            .jetstream
            .get_or_create_stream(config)
            .await
            .map_err(|e| ErgataiError::NatsError(format!("Failed to create stream: {}", e)))?;

        let stream_name = stream
            .info()
            .await
            .map(|i| i.config.name.clone())
            .unwrap_or_default();
        info!(stream = stream_name, "Stream created");
        Ok(stream)
    }

    /// Check if connection is still alive
    ///
    /// Uses the async-nats connection state to determine if the client is connected.
    /// Returns true if the client exists and hasn't been explicitly closed.
    /// Treats both Pending (initial connect) and Connected as healthy — only
    /// Disconnected means truly unusable.
    pub fn is_connected(&self) -> bool {
        matches!(
            self.client.connection_state(),
            async_nats::connection::State::Connected | async_nats::connection::State::Pending
        )
    }

    /// Check if connection is fully ready for operations
    ///
    /// Unlike `is_connected()`, this only returns true when the connection is
    /// in the `Connected` state — not during the initial `Pending` handshake.
    /// Use this for readiness probes that need to verify the connection can
    /// actually send/receive messages right now.
    pub fn is_ready(&self) -> bool {
        matches!(
            self.client.connection_state(),
            async_nats::connection::State::Connected
        )
    }

    /// Wait for connection to be ready (useful after server startup)
    pub async fn wait_for_ready(&self) -> ErgataiResult<()> {
        // Try a flush to verify the connection is actually working
        for attempt in 0..10 {
            match self.client.flush().await {
                Ok(()) => {
                    debug!(attempt = attempt, "NATS connection ready");
                    return Ok(());
                }
                Err(e) => {
                    debug!(attempt = attempt, error = %e, "NATS not ready, retrying");
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(ErgataiError::NatsError(
            "NATS connection not ready after 1 second".to_string(),
        ))
    }
}

/// Subject naming conventions for Ergatai
pub mod subjects {
    /// Task submission subject
    pub fn task_submit(pool_id: &str) -> String {
        format!("ergatai.task.submit.{}", pool_id)
    }

    /// Task completion notification
    pub fn task_complete(task_id: &str) -> String {
        format!("ergatai.task.complete.{}", task_id)
    }

    /// Task failure notification
    pub fn task_fail(task_id: &str) -> String {
        format!("ergatai.task.fail.{}", task_id)
    }

    /// DAG node ready notification
    pub fn dag_node_ready(dag_id: &str) -> String {
        format!("ergatai.dag.node_ready.{}", dag_id)
    }

    /// DAG node completion notification
    pub fn dag_node_complete(node_id: &str) -> String {
        format!("ergatai.dag.node_complete.{}", node_id)
    }

    /// DAG completion notification
    pub fn dag_complete(dag_id: &str) -> String {
        format!("ergatai.dag.complete.{}", dag_id)
    }

    /// Agent spawned notification
    pub fn agent_spawned(agent_id: &str) -> String {
        format!("ergatai.agent.spawned.{}", agent_id)
    }

    /// Agent stopped notification
    pub fn agent_stopped(agent_id: &str) -> String {
        format!("ergatai.agent.stopped.{}", agent_id)
    }

    /// Agent-to-agent message subject
    ///
    /// Used for bidirectional communication between agents.
    /// Example: `ergatai.agent.message.codex` for messages sent to the codex agent.
    pub fn agent_message(agent_id: &str) -> String {
        format!("ergatai.agent.message.{}", agent_id)
    }

    /// Wildcard subject for all agent messages
    pub fn all_agent_messages() -> &'static str {
        "ergatai.agent.message.*"
    }

    /// Wildcard subject for all task events
    pub fn all_tasks() -> &'static str {
        "ergatai.task.*"
    }

    /// Wildcard subject for all DAG events
    pub fn all_dag_events() -> &'static str {
        "ergatai.dag.>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subject_naming() {
        // Task subjects
        assert_eq!(subjects::task_submit("pool1"), "ergatai.task.submit.pool1");
        assert_eq!(
            subjects::task_complete("task123"),
            "ergatai.task.complete.task123"
        );
        assert_eq!(subjects::task_fail("task456"), "ergatai.task.fail.task456");

        // DAG subjects
        assert_eq!(
            subjects::dag_node_ready("dag1"),
            "ergatai.dag.node_ready.dag1"
        );
        assert_eq!(
            subjects::dag_node_complete("node1"),
            "ergatai.dag.node_complete.node1"
        );
        assert_eq!(subjects::dag_complete("dag1"), "ergatai.dag.complete.dag1");

        // Agent subjects
        assert_eq!(
            subjects::agent_spawned("agent1"),
            "ergatai.agent.spawned.agent1"
        );
        assert_eq!(
            subjects::agent_stopped("agent1"),
            "ergatai.agent.stopped.agent1"
        );

        // Wildcards
        assert_eq!(subjects::all_tasks(), "ergatai.task.*");
        assert_eq!(subjects::all_dag_events(), "ergatai.dag.>");
    }

    /// Test connection establishment and pub/sub
    /// Skips if nats-server is not available.
    #[tokio::test]
    async fn test_connection_and_pubsub() {
        use futures_util::StreamExt;

        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = NatsConnection::connect_to_server(server).await.unwrap();
        assert!(conn.is_connected());

        // Subscribe first
        let mut sub = conn.subscribe("test.pubsub").await.unwrap();

        // Publish
        conn.publish("test.pubsub", b"hello nats".to_vec())
            .await
            .unwrap();

        // Receive with timeout
        let msg = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("Should receive message within timeout")
            .expect("Stream should yield a message");

        assert_eq!(&msg.payload[..], b"hello nats");
    }

    /// Test stream creation
    #[tokio::test]
    async fn test_create_stream() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = NatsConnection::connect_to_server(server).await.unwrap();

        let config = Config {
            name: "TEST_STREAM".to_string(),
            subjects: vec!["test.stream.>".to_string()],
            ..Default::default()
        };

        let mut stream = match conn.create_stream(config).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (JetStream storage unavailable): {}", e);
                return;
            }
        };
        let info = stream.info().await.unwrap();
        assert_eq!(info.config.name, "TEST_STREAM");
    }

    #[test]
    fn test_subject_naming_agent_message() {
        assert_eq!(
            subjects::agent_message("codex"),
            "ergatai.agent.message.codex"
        );
        assert_eq!(
            subjects::agent_message("claude-code"),
            "ergatai.agent.message.claude-code"
        );
    }

    #[test]
    fn test_subject_naming_all_agent_messages() {
        assert_eq!(subjects::all_agent_messages(), "ergatai.agent.message.*");
    }

    #[test]
    fn test_subject_naming_consistency() {
        // Verify all subjects start with "ergatai."
        assert!(subjects::task_submit("pool1").starts_with("ergatai."));
        assert!(subjects::task_complete("t1").starts_with("ergatai."));
        assert!(subjects::task_fail("t1").starts_with("ergatai."));
        assert!(subjects::dag_node_ready("d1").starts_with("ergatai."));
        assert!(subjects::dag_node_complete("n1").starts_with("ergatai."));
        assert!(subjects::dag_complete("d1").starts_with("ergatai."));
        assert!(subjects::agent_spawned("a1").starts_with("ergatai."));
        assert!(subjects::agent_stopped("a1").starts_with("ergatai."));
        assert!(subjects::agent_message("a1").starts_with("ergatai."));
    }

    /// Test connection is_connected and is_ready states
    #[tokio::test]
    async fn test_connection_states() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = NatsConnection::connect_to_server(server).await.unwrap();

        // After successful connection, should be both connected and ready
        assert!(conn.is_connected(), "Should be connected");
        assert!(conn.is_ready(), "Should be ready");
    }

    /// Test wait_for_ready succeeds on active connection
    #[tokio::test]
    async fn test_wait_for_ready() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = NatsConnection::connect_to_server(server).await.unwrap();
        let result = conn.wait_for_ready().await;
        assert!(result.is_ok(), "Should become ready");
    }

    /// Test publish to non-existent subject still succeeds (fire-and-forget)
    #[tokio::test]
    async fn test_publish_non_existent_subject() {
        let server = match crate::shared_test_server().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = NatsConnection::connect_to_server(server).await.unwrap();

        // Core NATS publish to non-existent subject should succeed (no subscribers)
        let result = conn
            .publish("test.nonexistent.subject", b"data".to_vec())
            .await;
        assert!(
            result.is_ok(),
            "Publish to non-existent subject should succeed"
        );
    }
}
