//! NATS event bus — publish/subscribe helpers for DAG events
//!
//! Wraps `NatsConnection` with typed publish/subscribe methods for each
//! DAG event payload.  Handles JSON serialization and subject naming.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{ErgataiError, ErgataiResult};
use crate::nats::connection::NatsConnection;
use crate::nats::events::*;

/// Event bus for typed NATS pub/sub
///
/// Thin wrapper that handles serialization + subject naming so callers
/// don't need to construct subjects manually.
#[derive(Clone)]
pub struct EventBus {
    connection: NatsConnection,
}

impl EventBus {
    /// Create a new event bus from an existing NATS connection
    pub fn new(connection: NatsConnection) -> Self {
        Self { connection }
    }

    /// Get the underlying connection
    pub fn connection(&self) -> &NatsConnection {
        &self.connection
    }

    // ── Publish helpers ──

    /// Publish a task submission event
    pub async fn publish_task_submit(&self, payload: &TaskSubmitPayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.task.submit.{}", sanitize_agent_name(&payload.target_agent));
        self.publish(&subject, payload).await
    }

    /// Publish a node completion event
    pub async fn publish_node_complete(&self, payload: &NodeCompletePayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.dag.node_complete.{}", payload.node_id);
        self.publish(&subject, payload).await
    }

    /// Publish a node failure event
    pub async fn publish_node_failed(&self, payload: &NodeFailedPayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.dag.node_failed.{}", payload.node_id);
        self.publish(&subject, payload).await
    }

    /// Publish a DAG completion event
    pub async fn publish_dag_complete(&self, payload: &DagCompletePayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.dag.complete.{}", payload.dag_id);
        self.publish(&subject, payload).await
    }

    /// Publish an agent-to-agent message
    ///
    /// Routes the message to the target agent's inbox subject.
    /// Example: message to "codex" → `ergatai.agent.message.codex`
    pub async fn publish_agent_message(&self, payload: &AgentMessagePayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.agent.message.{}", sanitize_agent_name(&payload.to_agent));
        self.publish(&subject, payload).await
    }

    // ── Subscribe helpers ──

    /// Subscribe to task submission events for a specific agent
    pub async fn subscribe_task_submit(&self, agent_name: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.task.submit.{}", sanitize_agent_name(agent_name));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to task submission events for ALL agents (wildcard)
    pub async fn subscribe_all_task_submits(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.task.submit.*").await
    }

    /// Subscribe to node completion events for a specific node
    pub async fn subscribe_node_complete(&self, node_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.node_complete.{}", node_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL node completion events (wildcard)
    pub async fn subscribe_all_node_complete(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.dag.node_complete.*").await
    }

    /// Subscribe to node failure events for a specific node
    pub async fn subscribe_node_failed(&self, node_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.node_failed.{}", node_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL node failure events (wildcard)
    pub async fn subscribe_all_node_failed(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.dag.node_failed.*").await
    }

    /// Subscribe to DAG completion events
    pub async fn subscribe_dag_complete(&self, dag_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.dag.complete.{}", dag_id);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL DAG events (wildcard)
    pub async fn subscribe_all_dag_events(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.dag.*").await
    }

    /// Subscribe to messages for a specific agent
    ///
    /// Example: `subscribe_agent_message("codex")` subscribes to `ergatai.agent.message.codex`
    pub async fn subscribe_agent_message(&self, agent_name: &str) -> ErgataiResult<async_nats::Subscriber> {
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
    pub async fn publish_file_access_request(&self, payload: &FileAccessRequestPayload) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.request", payload).await
    }

    /// Publish a file access grant (to specific agent)
    pub async fn publish_file_access_grant(&self, payload: &FileAccessGrantPayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.access.grant.{}", sanitize_agent_name(&payload.agent_id));
        self.publish(&subject, payload).await
    }

    /// Publish a file access deny (to specific agent)
    pub async fn publish_file_access_deny(&self, payload: &FileAccessDenyPayload) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.access.deny.{}", sanitize_agent_name(&payload.agent_id));
        self.publish(&subject, payload).await
    }

    /// Publish a file access escalation (to main agent)
    pub async fn publish_file_access_escalate(&self, payload: &FileAccessEscalatePayload, main_agent_id: &str) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.access.escalate.{}", sanitize_agent_name(main_agent_id));
        self.publish(&subject, payload).await
    }

    /// Publish a file access approval (from main agent)
    pub async fn publish_file_access_approve(&self, payload: &FileAccessApprovePayload) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.approve", payload).await
    }

    /// Publish a file access rejection (from main agent)
    pub async fn publish_file_access_reject(&self, payload: &FileAccessRejectPayload) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.reject", payload).await
    }

    /// Publish a file access release
    pub async fn publish_file_access_release(&self, payload: &FileAccessReleasePayload) -> ErgataiResult<()> {
        self.publish("ergatai.file.access.release", payload).await
    }

    /// Publish a file access revocation (to specific agent)
    pub async fn publish_file_access_revoke(&self, payload: &FileAccessRevokePayload, agent_id: &str) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.access.revoke.{}", sanitize_agent_name(agent_id));
        self.publish(&subject, payload).await
    }

    /// Publish a file conflict arbitration request (to main agent)
    pub async fn publish_file_conflict_arbitrate(&self, payload: &FileConflictArbitratePayload, main_agent_id: &str) -> ErgataiResult<()> {
        let subject = format!("ergatai.file.conflict.arbitrate.{}", sanitize_agent_name(main_agent_id));
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
        let subject = format!("ergatai.system.token.{}", sanitize_agent_name(&payload.agent_id));
        self.publish(&subject, payload).await
    }

    // ── File Access Control subscribe helpers ──

    /// Subscribe to file access requests (FileLockManager)
    pub async fn subscribe_file_access_request(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.access.request").await
    }

    /// Subscribe to file access grants for a specific agent
    pub async fn subscribe_file_access_grant(&self, agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.access.grant.{}", sanitize_agent_name(agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to ALL file access grants (wildcard)
    pub async fn subscribe_all_file_access_grants(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.access.grant.*").await
    }

    /// Subscribe to file access denials for a specific agent
    pub async fn subscribe_file_access_deny(&self, agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.access.deny.{}", sanitize_agent_name(agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file access escalations (Main Agent)
    pub async fn subscribe_file_access_escalate(&self, main_agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.access.escalate.{}", sanitize_agent_name(main_agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file access approvals (FileLockManager)
    pub async fn subscribe_file_access_approve(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.access.approve").await
    }

    /// Subscribe to file access rejections (FileLockManager)
    pub async fn subscribe_file_access_reject(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.access.reject").await
    }

    /// Subscribe to file access releases (FileLockManager)
    pub async fn subscribe_file_access_release(&self) -> ErgataiResult<async_nats::Subscriber> {
        self.connection.subscribe("ergatai.file.access.release").await
    }

    /// Subscribe to file access revocations for a specific agent
    pub async fn subscribe_file_access_revoke(&self, agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.access.revoke.{}", sanitize_agent_name(agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file conflict arbitration (Main Agent)
    pub async fn subscribe_file_conflict_arbitrate(&self, main_agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
        let subject = format!("ergatai.file.conflict.arbitrate.{}", sanitize_agent_name(main_agent_id));
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file ready notifications for a specific file
    pub async fn subscribe_file_ready(&self, file_path: &str) -> ErgataiResult<async_nats::Subscriber> {
        let file_hash = format!("{:x}", md5::compute(file_path.as_bytes()));
        let subject = format!("ergatai.file.ready.{}", file_hash);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to file error notifications for a specific file
    pub async fn subscribe_file_error(&self, file_path: &str) -> ErgataiResult<async_nats::Subscriber> {
        let file_hash = format!("{:x}", md5::compute(file_path.as_bytes()));
        let subject = format!("ergatai.file.error.{}", file_hash);
        self.connection.subscribe(&subject).await
    }

    /// Subscribe to system token issuance for a specific agent
    pub async fn subscribe_system_token(&self, agent_id: &str) -> ErgataiResult<async_nats::Subscriber> {
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
pub async fn receive_event<T: for<'de> Deserialize<'de>>(
    subscriber: &mut async_nats::Subscriber,
) -> Option<ErgataiResult<T>>
where
    T: std::marker::Unpin,
{
    use futures_util::StreamExt;

    match subscriber.next().await {
        Some(msg) => {
            match serde_json::from_slice::<T>(&msg.payload) {
                Ok(payload) => Some(Ok(payload)),
                Err(e) => {
                    warn!(error = %e, "Failed to deserialize NATS event");
                    Some(Err(ErgataiError::json_with_source("Failed to deserialize NATS event", e)))
                }
            }
        }
        None => None, // Stream closed
    }
}

/// Sanitize agent name for use in NATS subject (replace non-alphanumeric with _)
fn sanitize_agent_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_sanitize_agent_name() {
        assert_eq!(sanitize_agent_name("claude-code"), "claude-code");
        assert_eq!(sanitize_agent_name("my_agent"), "my_agent");
        assert_eq!(sanitize_agent_name("agent.name.v2"), "agent_name_v2");
        assert_eq!(sanitize_agent_name("a/b/c"), "a_b_c");
    }

    /// Test publish + receive roundtrip for TaskSubmitPayload
    #[tokio::test]
    async fn test_event_bus_task_submit_roundtrip() {
        let server = match crate::nats::NatsServer::start().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::nats::NatsConnection::connect_to_server(&server).await.unwrap();
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
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                receive_event(&mut sub),
            )
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
        let server = match crate::nats::NatsServer::start().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::nats::NatsConnection::connect_to_server(&server).await.unwrap();
        let bus = EventBus::new(conn);

        let mut sub = bus.subscribe_all_node_complete().await.unwrap();

        let mut outputs = HashMap::new();
        outputs.insert("result".to_string(), "done".to_string());

        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "agent".to_string(),
            result_summary: Some("ok".to_string()),
            outputs,
            result_file: None,
        };

        bus.publish_node_complete(&payload).await.unwrap();

        let received: NodeCompletePayload =
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                receive_event(&mut sub),
            )
            .await
            .expect("timeout")
            .expect("stream closed")
            .expect("deser failed");

        assert_eq!(received.node_id, "n1");
        assert_eq!(received.outputs.get("result"), Some(&"done".to_string()));
    }

    /// Test NodeFailed publish + receive
    #[tokio::test]
    async fn test_event_bus_node_failed_roundtrip() {
        let server = match crate::nats::NatsServer::start().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let conn = crate::nats::NatsConnection::connect_to_server(&server).await.unwrap();
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
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                receive_event(&mut sub),
            )
            .await
            .expect("timeout")
            .expect("stream closed")
            .expect("deser failed");

        assert_eq!(received.node_id, "n2");
        assert_eq!(received.error, "crash");
        assert!(!received.retryable);
    }
}
