//! NATS event bus — publish/subscribe helpers for DAG events
//!
//! Wraps `NatsConnection` with typed publish/subscribe methods for each
//! DAG event payload.  Handles JSON serialization and subject naming.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::connection::NatsConnection;
use crate::events::*;
use ergatai_error::{ErgataiError, ErgataiResult};

/// Cache for the last backpressure check result. Avoids calling
/// `stream.info()` per message — re-queries only if older than 5s.
struct BackpressureCache {
    last_check: Instant,
    last_depth: u64,
}

/// How long to trust a cached backpressure depth before re-querying NATS.
const BACKPRESSURE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Returns true if the cache should be refreshed (stale or empty).
fn should_requery(cache: &BackpressureCache, now: Instant) -> bool {
    now.duration_since(cache.last_check) > BACKPRESSURE_CACHE_TTL
}

/// Event bus for typed NATS pub/sub
///
/// Thin wrapper that handles serialization + subject naming so callers
/// don't need to construct subjects manually.
#[derive(Clone)]
pub struct EventBus {
    connection: NatsConnection,
    backpressure_cache: Arc<Mutex<BackpressureCache>>,
}

impl EventBus {
    /// Create a new event bus from an existing NATS connection
    pub fn new(connection: NatsConnection) -> Self {
        Self {
            connection,
            backpressure_cache: Arc::new(Mutex::new(BackpressureCache {
                last_check: Instant::now() - BACKPRESSURE_CACHE_TTL, // force first re-query
                last_depth: 0,
            })),
        }
    }

    /// Get the underlying connection
    pub fn connection(&self) -> &NatsConnection {
        &self.connection
    }

    // ── Publish helpers ──

    /// Publish a task submission event via JetStream (reliable, persisted)
    ///
    /// Routes to `ergatai.task.submit.{target_agent}` on the `DAG_EVENTS` stream.
    /// TaskScheduler pulls from this stream with a filtered consumer.
    /// Returns `PublishAck` with stream/sequence for traceability.
    pub async fn publish_task_submit(
        &self,
        payload: &TaskSubmitPayload,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        let subject = format!(
            "ergatai.task.submit.{}",
            sanitize_agent_name(&payload.target_agent)
        );
        let json = serde_json::to_vec(payload)?;
        self.connection.publish_jetstream(&subject, json).await
    }

    /// Publish a node completion event via JetStream (reliable, persisted)
    ///
    /// Routes to `ergatai.dag.node_complete.{node_id}` on the `DAG_EVENTS` stream.
    /// DagScheduler pulls from this stream with the `dag_events` consumer.
    pub async fn publish_node_complete(
        &self,
        payload: &NodeCompletePayload,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        let subject = format!("ergatai.dag.node_complete.{}", payload.node_id);
        let json = serde_json::to_vec(payload)?;
        self.connection.publish_jetstream(&subject, json).await
    }

    /// Publish a node failure event via JetStream (reliable, persisted)
    ///
    /// Routes to `ergatai.dag.node_failed.{node_id}` on the `DAG_EVENTS` stream.
    /// DagScheduler pulls from this stream with the `dag_events` consumer.
    pub async fn publish_node_failed(
        &self,
        payload: &NodeFailedPayload,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        let subject = format!("ergatai.dag.node_failed.{}", payload.node_id);
        let json = serde_json::to_vec(payload)?;
        self.connection.publish_jetstream(&subject, json).await
    }

    /// Publish a DAG completion event via JetStream (reliable, persisted)
    ///
    /// Routes to `ergatai.dag.complete.{dag_id}` on the `DAG_EVENTS` stream.
    /// Observers pull from this stream with the `dag_events` consumer.
    pub async fn publish_dag_complete(
        &self,
        payload: &DagCompletePayload,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        let subject = format!("ergatai.dag.complete.{}", payload.dag_id);
        let json = serde_json::to_vec(payload)?;
        self.connection.publish_jetstream(&subject, json).await
    }

    /// Informational log for a node that has consumed 50% of its timeout budget.
    ///
    /// MVP: log-only — no NATS round-trip. The warn/escalate tiers exist so
    /// operators can grep the logs for early signals of a node about to fail.
    /// Phase 3 may promote this to a real NATS subject for dashboards.
    pub async fn publish_node_warned(
        &self,
        dag_id: &str,
        node_id: &str,
        elapsed_secs: u64,
    ) -> ErgataiResult<()> {
        tracing::warn!(
            dag_id = dag_id,
            node_id = node_id,
            elapsed_secs = elapsed_secs,
            "node timeout warning — 50% of budget consumed"
        );
        Ok(())
    }

    /// Informational log for a node that has consumed 80% of its timeout budget.
    ///
    /// Logged at ERROR level so it surfaces in production log aggregation even
    /// when WARN is filtered. MVP: log-only — no NATS round-trip.
    pub async fn publish_node_escalated(
        &self,
        dag_id: &str,
        node_id: &str,
        elapsed_secs: u64,
    ) -> ErgataiResult<()> {
        tracing::error!(
            dag_id = dag_id,
            node_id = node_id,
            elapsed_secs = elapsed_secs,
            "node timeout escalated — 80% of budget consumed"
        );
        Ok(())
    }

    /// Publish an agent-to-agent message
    ///
    /// Routes the message to the target agent's inbox subject.
    /// Example: message to "codex" → `ergatai.agent.message.codex`
    pub async fn publish_agent_message(&self, payload: &AgentMessagePayload) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.agent.message.{}",
            sanitize_agent_name(&payload.to_agent)
        );
        self.publish(&subject, payload).await
    }

    /// Check whether the AGENT_MESSAGES stream is under the backpressure threshold.
    /// Returns Ok(()) if under threshold, or Err if the stream is overloaded.
    /// Caches the last check result for 5s to avoid per-message NATS round-trips.
    ///
    /// # Concurrency
    ///
    /// Drops the cache lock before the NATS round-trip (`get_stream` + `info`),
    /// so concurrent callers are not serialized behind the await. A second
    /// check after re-acquiring the lock prevents overwriting a fresher result
    /// written by another caller that raced us.
    pub async fn check_backpressure(&self) -> ErgataiResult<()> {
        let threshold = std::env::var("ERGATAI_BACKPRESSURE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::agent_message_stream::BACKPRESSURE_THRESHOLD);

        // Fast path: cache is fresh — return cached depth without any I/O.
        let cached_depth = {
            let cache = self.backpressure_cache.lock().await;
            let now = Instant::now();
            if !should_requery(&cache, now) {
                Some(cache.last_depth)
            } else {
                None
            }
        }; // lock released before await

        let depth = if let Some(d) = cached_depth {
            d
        } else {
            // Cache is stale — query NATS without holding the lock.
            let fresh_depth = self.connection.agent_messages_pending_count().await?;

            // Re-acquire lock and check if another caller already refreshed
            // while we were waiting. If they did, use their value (which is
            // fresher or at least equally fresh). If not, store ours.
            let mut cache = self.backpressure_cache.lock().await;
            let now = Instant::now();
            if should_requery(&cache, now) {
                cache.last_check = now;
                cache.last_depth = fresh_depth;
                fresh_depth
            } else {
                cache.last_depth
            }
        };

        if depth >= threshold {
            Err(ErgataiError::NatsError(format!(
                "backpressure: AGENT_MESSAGES stream has {depth} pending (threshold {threshold})"
            )))
        } else {
            Ok(())
        }
    }

    /// Publish an agent-to-agent message via JetStream (reliable, persisted)
    ///
    /// Same subject routing as [`publish_agent_message`](Self::publish_agent_message),
    /// but uses JetStream so the message is durably stored before returning.
    /// The consumer pulls from the `AGENT_MESSAGES` stream and delivers via
    /// tmux injection / MCP notification.
    ///
    /// # Returns
    ///
    /// `PublishAck` with the stream sequence number on success.
    /// Callers should treat `Err` as "NATS unavailable" and may fall back to
    /// direct delivery or surface the error.
    pub async fn publish_agent_message_reliable(
        &self,
        payload: &AgentMessagePayload,
    ) -> ErgataiResult<async_nats::jetstream::publish::PublishAck> {
        // Backpressure check: refuse publish if stream is overloaded.
        self.check_backpressure().await?;

        let subject = format!(
            "ergatai.agent.message.{}",
            sanitize_agent_name(&payload.to_agent)
        );
        let json = serde_json::to_vec(payload)?;
        self.connection.publish_jetstream(&subject, json).await
    }

    // ── Subscribe helpers ──

    /// Subscribe to task submission events for a specific agent
    pub async fn subscribe_task_submit(
        &self,
        agent_name: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.task.submit.{}", sanitize_agent_name(agent_name));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to task submission events for ALL agents (wildcard)
    pub async fn subscribe_all_task_submits(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.task.submit.*").await
    }

    /// Subscribe to node completion events for a specific node
    pub async fn subscribe_node_complete(
        &self,
        node_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.node_complete.{}", node_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL node completion events (wildcard)
    pub async fn subscribe_all_node_complete(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.dag.node_complete.*")
            .await
    }

    /// Subscribe to node failure events for a specific node
    pub async fn subscribe_node_failed(
        &self,
        node_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.node_failed.{}", node_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL node failure events (wildcard)
    pub async fn subscribe_all_node_failed(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.dag.node_failed.*").await
    }

    /// Subscribe to DAG completion events
    pub async fn subscribe_dag_complete(
        &self,
        dag_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.complete.{}", dag_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL DAG events (wildcard)
    pub async fn subscribe_all_dag_events(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.dag.>").await
    }

    /// Subscribe to messages for a specific agent
    ///
    /// Example: `subscribe_agent_message("codex")` subscribes to `ergatai.agent.message.codex`
    pub async fn subscribe_agent_message(
        &self,
        agent_name: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.agent.message.{}", sanitize_agent_name(agent_name));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL agent messages (wildcard)
    ///
    /// Useful for a central router that forwards messages to the appropriate ACP session.
    pub async fn subscribe_all_agent_messages(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.agent.message.*").await
    }

    // ── File Access Control publish helpers ──

    /// Publish a file access request
    pub async fn publish_file_access_request(
        &self,
        payload: &FileAccessRequestPayload,
    ) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.request", payload).await
    }

    /// Publish a file access grant (to specific agent)
    pub async fn publish_file_access_grant(
        &self,
        payload: &FileAccessGrantPayload,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.file.access.grant.{}",
            sanitize_agent_name(&payload.agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a file access deny (to specific agent)
    pub async fn publish_file_access_deny(
        &self,
        payload: &FileAccessDenyPayload,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.file.access.deny.{}",
            sanitize_agent_name(&payload.agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a file access escalation (to main agent)
    pub async fn publish_file_access_escalate(
        &self,
        payload: &FileAccessEscalatePayload,
        main_agent_id: &str,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.file.access.escalate.{}",
            sanitize_agent_name(main_agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a file access approval (from main agent)
    pub async fn publish_file_access_approve(
        &self,
        payload: &FileAccessApprovePayload,
    ) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.approve", payload).await
    }

    /// Publish a file access rejection (from main agent)
    pub async fn publish_file_access_reject(
        &self,
        payload: &FileAccessRejectPayload,
    ) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.reject", payload).await
    }

    /// Publish a file access release
    pub async fn publish_file_access_release(
        &self,
        payload: &FileAccessReleasePayload,
    ) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.release", payload).await
    }

    /// Publish a file access revocation (to specific agent)
    pub async fn publish_file_access_revoke(
        &self,
        payload: &FileAccessRevokePayload,
        agent_id: &str,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.file.access.revoke.{}",
            sanitize_agent_name(agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a file conflict arbitration request (to main agent)
    pub async fn publish_file_conflict_arbitrate(
        &self,
        payload: &FileConflictArbitratePayload,
        main_agent_id: &str,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.file.conflict.arbitrate.{}",
            sanitize_agent_name(main_agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a file ready notification
    pub async fn publish_file_ready(&self, payload: &FileReadyPayload) -> ErgataiResult<()> {
        // Use file path hash for subject (avoid special characters)
        let file_hash = format!("{:x}", md5::compute(payload.file_path.as_bytes()));
        let subject = format!("ergatai.file.ready.{}", file_hash);
        self.publish(&subject, payload).await
    }

    /// Publish a file error notification
    pub async fn publish_file_error(&self, payload: &FileErrorPayload) -> ErgataiResult<()> {
        let file_hash = format!("{:x}", md5::compute(payload.file_path.as_bytes()));
        let subject = format!("ergatai.file.error.{}", file_hash);
        self.publish(&subject, payload).await
    }

    /// Publish a system token issuance
    pub async fn publish_system_token(&self, payload: &SystemTokenPayload) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.system.token.{}",
            sanitize_agent_name(&payload.agent_id)
        );
        self.publish(&subject, payload).await
    }

    /// Publish a kernel-level enforcement event (fanotify decision).
    ///
    /// Subject: `ergatai.file.enforce.{project_id}` on the FILE_EVENTS stream.
    pub async fn publish_file_enforcement(
        &self,
        project_id: &str,
        payload: &crate::events::FileEnforcementPayload,
    ) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.enforce.{}", sanitize_agent_name(project_id));
        self.publish(&subject, payload).await
    }

    /// Publish an agent lifecycle state change event.
    ///
    /// Subject: `ergatai.agent.lifecycle.{agent_uuid}`
    /// Uses core NATS (not JetStream) for low-latency fan-out to subscribers.
    /// TaskScheduler and other observers subscribe to react to agent state changes.
    pub async fn publish_agent_lifecycle(
        &self,
        payload: &AgentLifecycleEventPayload,
    ) -> ErgataiResult<()> {
        let subject = format!(
            "ergatai.agent.lifecycle.{}",
            sanitize_agent_name(&payload.agent_uuid)
        );
        self.publish(&subject, payload).await
    }

    /// Subscribe to agent lifecycle events for a specific agent.
    pub async fn subscribe_agent_lifecycle(
        &self,
        agent_uuid: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!(
            "ergatai.agent.lifecycle.{}",
            sanitize_agent_name(agent_uuid)
        );
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL agent lifecycle events (wildcard).
    pub async fn subscribe_all_agent_lifecycles(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.agent.lifecycle.*").await
    }

    // ── File Access Control subscribe helpers ──

    /// Subscribe to file access requests (FileLockManager)
    pub async fn subscribe_file_access_request(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.file.access.request")
            .await
    }

    /// Subscribe to file access grants for a specific agent
    pub async fn subscribe_file_access_grant(
        &self,
        agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!(
            "ergatai.file.access.grant.{}",
            sanitize_agent_name(agent_id)
        );
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL file access grants (wildcard)
    pub async fn subscribe_all_file_access_grants(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.file.access.grant.*")
            .await
    }

    /// Subscribe to file access denials for a specific agent
    pub async fn subscribe_file_access_deny(
        &self,
        agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.access.deny.{}", sanitize_agent_name(agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file access escalations (Main Agent)
    pub async fn subscribe_file_access_escalate(
        &self,
        main_agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!(
            "ergatai.file.access.escalate.{}",
            sanitize_agent_name(main_agent_id)
        );
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file access approvals (FileLockManager)
    pub async fn subscribe_file_access_approve(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.file.access.approve")
            .await
    }

    /// Subscribe to file access rejections (FileLockManager)
    pub async fn subscribe_file_access_reject(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.file.access.reject")
            .await
    }

    /// Subscribe to file access releases (FileLockManager)
    pub async fn subscribe_file_access_release(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection
            .subscribe("ergatai.file.access.release")
            .await
    }

    /// Subscribe to file access revocations for a specific agent
    pub async fn subscribe_file_access_revoke(
        &self,
        agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!(
            "ergatai.file.access.revoke.{}",
            sanitize_agent_name(agent_id)
        );
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file conflict arbitration (Main Agent)
    pub async fn subscribe_file_conflict_arbitrate(
        &self,
        main_agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!(
            "ergatai.file.conflict.arbitrate.{}",
            sanitize_agent_name(main_agent_id)
        );
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file ready notifications for a specific file
    pub async fn subscribe_file_ready(
        &self,
        file_path: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let file_hash = format!("{:x}", md5::compute(file_path.as_bytes()));
        let subject = format!("ergatai.file.ready.{}", file_hash);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file error notifications for a specific file
    pub async fn subscribe_file_error(
        &self,
        file_path: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let file_hash = format!("{:x}", md5::compute(file_path.as_bytes()));
        let subject = format!("ergatai.file.error.{}", file_hash);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to system token issuance for a specific agent
    pub async fn subscribe_system_token(
        &self,
        agent_id: &str,
    ) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.system.token.{}", sanitize_agent_name(agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL file access events (wildcard)
    pub async fn subscribe_all_file_access_events(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.>").await
    }

    // ── Generic publish ──

    /// Publish a typed payload to a subject (JSON serialized)
    async fn publish<T: Serialize>(&self, subject: &str, payload: &T) -> ErgataiResult<()> {
        let json = serde_json::to_vec(payload)?;

        self.connection.publish(subject, json).await?;
        debug!(subject = subject, "Published event");
        Ok(())
    }
}

/// Receive and deserialize a typed event from a subscriber.
///
/// Returns `None` on timeout, `Err` on deserialization failure.
pub async fn receive_event<T>(subscriber: &mut async_nats::Subscriber) -> Option<ErgataiResult<T>>
where
    T: for<'de> Deserialize<'de> + std::marker::Unpin,
{
    use futures_util::StreamExt;

    match subscriber.next().await {
        Some(msg) => match serde_json::from_slice::<T>(&msg.payload) {
            Ok(payload) => Some(Ok(payload)),
            Err(e) => {
                warn!(error = %e, "Failed to deserialize NATS event");
                Some(Err(ErgataiError::json_with_source(
                    "Failed to deserialize NATS event",
                    e,
                )))
            }
        },
        None => None, // Stream closed
    }
}

/// Sanitize agent name for use in NATS subject (replace non-alphanumeric with _)
fn sanitize_agent_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn backpressure_cache_fresh_skips_requery() {
        let cache = BackpressureCache {
            last_check: Instant::now(),
            last_depth: 500,
        };
        assert!(!should_requery(&cache, Instant::now()));
    }

    #[test]
    fn backpressure_cache_stale_triggers_requery() {
        let cache = BackpressureCache {
            last_check: Instant::now() - Duration::from_secs(10),
            last_depth: 500,
        };
        assert!(should_requery(&cache, Instant::now()));
    }

    #[test]
    fn test_sanitize_agent_name() {
        assert_eq!(sanitize_agent_name("claude-code"), "claude-code");
        assert_eq!(sanitize_agent_name("my_agent"), "my_agent");
        assert_eq!(sanitize_agent_name("agent.name.v2"), "agent_name_v2");
        assert_eq!(sanitize_agent_name("a/b/c"), "a_b_c");
    }

    /// Test full task queue flow: create → submit → consume → ack
    /// Skips if nats-server is not available.
    #[tokio::test]
    async fn test_event_bus_task_submit_roundtrip() {
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

        // Create or get the DAG_EVENTS stream (shared across tests)
        // Using get_or_create_stream via create_stream which handles existing streams gracefully
        let config = async_nats::jetstream::stream::Config {
            name: "DAG_EVENTS".to_string(),
            subjects: vec![
                "ergatai.task.submit.*".to_string(),
                "ergatai.dag.>".to_string(),
            ],
            ..Default::default()
        };
        // Ignore error if stream already exists with same config (from another test)
        if conn.create_stream(config).await.is_err() {
            eprintln!("⚠️  Skipping (JetStream storage unavailable)");
            return;
        }

        let bus = EventBus::new(conn);

        // Subscribe first
        let mut sub = bus.subscribe_task_submit("claude-code").await.unwrap();

        // Publish
        let payload = TaskSubmitPayload {
            task_id: "task-1".to_string(),
            plan_content: "# Plan\nDo stuff".to_string(),
            plan_file: ".ergatai/plans/task-1.md".to_string(),
            target_agent: "claude-code".to_string(),
            priority: 1,
            timeout_secs: Some(60),
            dag_id: Some("dag-1".to_string()),
        };

        bus.publish_task_submit(&payload).await.unwrap();

        // Receive
        let received: TaskSubmitPayload =
            tokio::time::timeout(std::time::Duration::from_secs(2), receive_event(&mut sub))
                .await
                .expect("timeout")
                .expect("stream closed")
                .expect("deserialization failed");

        assert_eq!(received.task_id, "task-1");
        assert_eq!(received.plan_content, "# Plan\nDo stuff");
        assert_eq!(received.timeout_secs, Some(60));
    }

    /// Test NodeComplete publish + receive
    #[tokio::test]
    async fn test_event_bus_node_complete_roundtrip() {
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

        // Create or get the DAG_EVENTS stream (shared across tests)
        let config = async_nats::jetstream::stream::Config {
            name: "DAG_EVENTS".to_string(),
            subjects: vec![
                "ergatai.task.submit.*".to_string(),
                "ergatai.dag.>".to_string(),
            ],
            ..Default::default()
        };
        if conn.create_stream(config).await.is_err() {
            eprintln!("⚠️  Skipping (JetStream storage unavailable)");
            return;
        }

        let bus = EventBus::new(conn);

        let mut sub = bus.subscribe_all_node_complete().await.unwrap();

        let mut outputs = serde_json::Map::new();
        outputs.insert(
            "result".to_string(),
            serde_json::Value::String("done".to_string()),
        );

        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "agent".to_string(),
            result_summary: Some("ok".to_string()),
            outputs: serde_json::Value::Object(outputs),
            result_file: None,
        };

        bus.publish_node_complete(&payload).await.unwrap();

        let received: NodeCompletePayload =
            tokio::time::timeout(std::time::Duration::from_secs(2), receive_event(&mut sub))
                .await
                .expect("timeout")
                .expect("stream closed")
                .expect("deser failed");

        assert_eq!(received.node_id, "n1");
        assert_eq!(
            received.outputs.get("result"),
            Some(&serde_json::Value::String("done".to_string()))
        );
    }

    /// Test NodeFailed publish + receive
    #[tokio::test]
    async fn test_event_bus_node_failed_roundtrip() {
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

        // Create or get the DAG_EVENTS stream (shared across tests)
        let config = async_nats::jetstream::stream::Config {
            name: "DAG_EVENTS".to_string(),
            subjects: vec![
                "ergatai.task.submit.*".to_string(),
                "ergatai.dag.>".to_string(),
            ],
            ..Default::default()
        };
        if conn.create_stream(config).await.is_err() {
            eprintln!("⚠️  Skipping (JetStream storage unavailable)");
            return;
        }

        let bus = EventBus::new(conn);

        let mut sub = bus.subscribe_all_node_failed().await.unwrap();

        let payload = NodeFailedPayload {
            node_id: "n2".to_string(),
            task_id: "n2".to_string(),
            agent_name: "codex".to_string(),
            error: "crash".to_string(),
            retryable: false,
        };

        bus.publish_node_failed(&payload).await.unwrap();

        let received: NodeFailedPayload =
            tokio::time::timeout(std::time::Duration::from_secs(2), receive_event(&mut sub))
                .await
                .expect("timeout")
                .expect("stream closed")
                .expect("deser failed");

        assert_eq!(received.node_id, "n2");
        assert_eq!(received.error, "crash");
        assert!(!received.retryable);
    }

    #[test]
    fn test_sanitize_agent_name_special_chars() {
        // Spaces become underscores
        assert_eq!(sanitize_agent_name("agent name"), "agent_name");
        // Dots become underscores
        assert_eq!(sanitize_agent_name("agent.v2.beta"), "agent_v2_beta");
        // Mixed characters
        assert_eq!(sanitize_agent_name("a/b.c@d!e"), "a_b_c_d_e");
        // Empty string
        assert_eq!(sanitize_agent_name(""), "");
        // Already clean
        assert_eq!(sanitize_agent_name("agent-1"), "agent-1");
    }

    #[test]
    fn test_subject_construction_task_submit() {
        let subject = format!("ergatai.task.submit.{}", sanitize_agent_name("claude-code"));
        assert_eq!(subject, "ergatai.task.submit.claude-code");

        let subject2 = format!(
            "ergatai.task.submit.{}",
            sanitize_agent_name("agent/with/slashes")
        );
        assert_eq!(subject2, "ergatai.task.submit.agent_with_slashes");
    }

    #[test]
    fn test_subject_construction_agent_message() {
        let subject = format!("ergatai.agent.message.{}", sanitize_agent_name("codex"));
        assert_eq!(subject, "ergatai.agent.message.codex");
    }

    #[test]
    fn test_subject_construction_file_access() {
        let subject = format!(
            "ergatai.file.access.grant.{}",
            sanitize_agent_name("agent-a")
        );
        assert_eq!(subject, "ergatai.file.access.grant.agent-a");
    }

    /// Test DagComplete publish + receive
    #[tokio::test]
    async fn test_event_bus_dag_complete_roundtrip() {
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

        // Try to create stream, skip if storage is insufficient
        let config = async_nats::jetstream::stream::Config {
            name: "DAG_EVENTS".to_string(),
            subjects: vec![
                "ergatai.task.submit.*".to_string(),
                "ergatai.dag.>".to_string(),
            ],
            ..Default::default()
        };
        if conn.create_stream(config).await.is_err() {
            eprintln!("⚠️  Skipping (JetStream storage unavailable)");
            return;
        }

        let bus = EventBus::new(conn);
        let mut sub = bus.subscribe_dag_complete("dag-test").await.unwrap();

        let payload = DagCompletePayload {
            dag_id: "dag-test".to_string(),
            total_nodes: 5,
            completed_nodes: 5,
            failed_nodes: 0,
            duration_secs: 42,
        };

        bus.publish_dag_complete(&payload).await.unwrap();

        let received: DagCompletePayload =
            tokio::time::timeout(std::time::Duration::from_secs(2), receive_event(&mut sub))
                .await
                .expect("timeout")
                .expect("stream closed")
                .expect("deser failed");

        assert_eq!(received.dag_id, "dag-test");
        assert_eq!(received.total_nodes, 5);
        assert_eq!(received.duration_secs, 42);
    }

    /// Test AgentMessage publish + receive
    #[tokio::test]
    async fn test_event_bus_agent_message_roundtrip() {
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

        let bus = EventBus::new(conn);
        let mut sub = bus.subscribe_agent_message("codex").await.unwrap();

        let payload = AgentMessagePayload {
            from_agent: "claude-code".to_string(),
            to_agent: "codex".to_string(),
            from_uuid: None,
            to_uuid: None,
            content: "@codex review this".to_string(),
            thread_id: Some("thread-1".to_string()),
            timestamp: 1234567890,
            metadata: HashMap::new(),
        };

        bus.publish_agent_message(&payload).await.unwrap();

        let received: AgentMessagePayload =
            tokio::time::timeout(std::time::Duration::from_secs(2), receive_event(&mut sub))
                .await
                .expect("timeout")
                .expect("stream closed")
                .expect("deser failed");

        assert_eq!(received.from_agent, "claude-code");
        assert_eq!(received.to_agent, "codex");
        assert_eq!(received.content, "@codex review this");
    }
}
