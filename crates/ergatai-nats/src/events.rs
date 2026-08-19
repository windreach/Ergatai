//! NATS event payload types for DAG event-driven communication
//!
//! Defines the message payloads published/subscribed via NATS subjects.
//! All payloads are wrapped in `TaskMessage<T>` (from `task_queue.rs`)
//! which adds message_id, correlation_id, timestamp, retry metadata.
//!
//! ## Subject mapping
//!
//! | Payload                      | Subject                                    | Publisher       | Subscriber    |
//! |------------------------------|--------------------------------------------|-----------------|---------------|
//! | `TaskSubmitPayload`          | `ergatai.task.submit.{target_agent}`       | DagScheduler    | TaskScheduler |
//! | `NodeCompletePayload`        | `ergatai.dag.node_complete.{node_id}`      | AgentLauncher   | DagScheduler  |
//! | `NodeFailedPayload`          | `ergatai.dag.node_failed.{node_id}`       | AgentLauncher   | DagScheduler  |
//! | `DagCompletePayload`         | `ergatai.dag.complete.{dag_id}`            | DagScheduler    | Observers     |
//! | `FileAccessRequestPayload`   | `ergatai.file.access.request`              | Agent           | FileLockMgr   |
//! | `FileAccessGrantPayload`     | `ergatai.file.access.grant.{agent_id}`     | FileLockMgr     | Agent         |
//! | `FileAccessDenyPayload`      | `ergatai.file.access.deny.{agent_id}`      | FileLockMgr     | Agent         |
//! | `FileAccessEscalatePayload`  | `ergatai.file.access.escalate.{main_id}`   | FileLockMgr     | MainAgent     |
//! | `FileAccessApprovePayload`   | `ergatai.file.access.approve`              | MainAgent       | FileLockMgr   |
//! | `FileAccessRejectPayload`    | `ergatai.file.access.reject`               | MainAgent       | FileLockMgr   |
//! | `FileAccessReleasePayload`   | `ergatai.file.access.release`              | Agent           | FileLockMgr   |
//! | `FileAccessRevokePayload`    | `ergatai.file.access.revoke.{agent_id}`    | MainAgent       | FileLockMgr   |
//! | `FileConflictArbitratePayload`| `ergatai.file.conflict.arbitrate.{main}`  | FileLockMgr     | MainAgent     |
//! | `FileReadyPayload`           | `ergatai.file.ready.{file_hash}`           | FileLockMgr     | Waiters       |
//! | `FileErrorPayload`           | `ergatai.file.error.{file_hash}`           | FileLockMgr     | Waiters       |
//! | `SystemTokenPayload`         | `ergatai.system.token.{agent_id}`          | System          | Agent         |

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Task submission: DagScheduler → TaskScheduler
///
/// Carries the rendered plan content inline so the TaskScheduler
/// doesn't need to read the file from disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSubmitPayload {
    /// Task ID (== node_id for DAG tasks)
    pub task_id: String,
    /// Rendered plan markdown content (inline, no file I/O needed)
    pub plan_content: String,
    /// Plan file path (kept for backup / debugging / agent reference)
    pub plan_file: String,
    /// Target agent name
    pub target_agent: String,
    /// Execution priority (1 = default, higher = more urgent)
    pub priority: u32,
    /// Optional timeout in seconds
    pub timeout_secs: Option<u64>,
    /// Correlation: which DAG this task belongs to
    pub dag_id: Option<String>,
}

/// Node completion: AgentLauncher → DagScheduler
///
/// Carries structured outputs so `DagContext.record_output()` can be
/// called directly from the NATS message — no file parsing needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCompletePayload {
    /// DAG node ID
    pub node_id: String,
    /// Task ID (== node_id)
    pub task_id: String,
    /// Agent that executed the task
    pub agent_name: String,
    /// Brief result summary (not the full result file)
    pub result_summary: Option<String>,
    /// Structured outputs (JSON value) → fed into DagContext for template rendering
    pub outputs: serde_json::Value,
    /// Optional path to the full result file (for large outputs)
    pub result_file: Option<String>,
}

/// Node failure: AgentLauncher → DagScheduler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeFailedPayload {
    /// DAG node ID
    pub node_id: String,
    /// Task ID (== node_id)
    pub task_id: String,
    /// Agent that failed
    pub agent_name: String,
    /// Error message
    pub error: String,
    /// Whether this failure is retryable
    pub retryable: bool,
}

/// DAG completion: DagScheduler → Observers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagCompletePayload {
    /// DAG identifier (project root hash or explicit ID)
    pub dag_id: String,
    /// Total number of nodes in the DAG
    pub total_nodes: u32,
    /// Number of successfully completed nodes
    pub completed_nodes: u32,
    /// Number of failed nodes
    pub failed_nodes: u32,
    /// Execution duration in seconds
    pub duration_secs: u64,
}

/// Agent-to-agent message: Agent A → Ergatai → Agent B
///
/// Enables bidirectional conversation between agents.
/// Ergatai acts as relay: detects @agent mentions in ACP messages,
/// routes via NATS, and forwards to the target agent's ACP session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessagePayload {
    /// Source agent ID (sender) - pane ID for backward compatibility
    pub from_agent: String,
    /// Target agent ID (receiver) - pane ID for backward compatibility
    pub to_agent: String,
    /// Source agent UUID (stable identifier, for routing)
    pub from_uuid: Option<String>,
    /// Target agent UUID (stable identifier, for routing)
    pub to_uuid: Option<String>,
    /// Message content (the text mentioning @target_agent)
    pub content: String,
    /// Optional: conversation thread ID (for multi-turn dialogs)
    pub thread_id: Option<String>,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
    /// Optional: structured data payload (JSON)
    pub metadata: HashMap<String, String>,
}

// ===== File Access Control Payloads =====

/// File access request: Agent → FileLockManager
///
/// Agent requests permission to access a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessRequestPayload {
    /// Unique request ID (for idempotency)
    pub request_id: String,
    /// Agent requesting access
    pub agent_id: String,
    /// ACP session ID
    pub session_id: String,
    /// File path (relative to project root)
    pub file_path: String,
    /// Access mode (READ or WRITE)
    pub mode: String,
    /// Reason for the request
    pub reason: Option<String>,
    /// DAG node ID (if part of a DAG task)
    pub node_id: Option<String>,
    /// Expected duration in seconds (for heartbeat timeout)
    pub expected_duration_secs: Option<u64>,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access grant: FileLockManager → Agent
///
/// System grants file access permission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessGrantPayload {
    /// Request ID (correlates with request)
    pub request_id: String,
    /// Granted token ID
    pub token_id: String,
    /// Agent ID
    pub agent_id: String,
    /// File path
    pub file_path: String,
    /// Access mode
    pub mode: String,
    /// Who approved (system or agent_id)
    pub approved_by: String,
    /// Token expiration timestamp
    pub expires_at: u64,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access deny: FileLockManager → Agent
///
/// System denies file access permission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessDenyPayload {
    /// Request ID (correlates with request)
    pub request_id: String,
    /// Agent ID
    pub agent_id: String,
    /// File path
    pub file_path: String,
    /// Denial reason
    pub reason: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access escalate: FileLockManager → Main Agent
///
/// System escalates approval decision to main agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessEscalatePayload {
    /// Request ID
    pub request_id: String,
    /// Requesting agent ID
    pub agent_id: String,
    /// File path
    pub file_path: String,
    /// Access mode
    pub mode: String,
    /// Reason for request
    pub reason: Option<String>,
    /// Conflict info (if conflict with another agent)
    pub conflict_with: Option<String>,
    /// Timeout for decision (seconds)
    pub timeout_secs: u64,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access approve: Main Agent → FileLockManager
///
/// Main agent approves a file access request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessApprovePayload {
    /// Request ID
    pub request_id: String,
    /// Approving agent ID (main agent)
    pub approver_id: String,
    /// Optional: custom scope (if expanding)
    pub custom_scope: Option<String>,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access reject: Main Agent → FileLockManager
///
/// Main agent rejects a file access request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessRejectPayload {
    /// Request ID
    pub request_id: String,
    /// Rejecting agent ID (main agent)
    pub rejecter_id: String,
    /// Rejection reason
    pub reason: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access release: Agent → FileLockManager
///
/// Agent releases a file lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessReleasePayload {
    /// Token ID to release
    pub token_id: String,
    /// Agent ID
    pub agent_id: String,
    /// File path
    pub file_path: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File access revoke: Main Agent → FileLockManager → Agent
///
/// Main agent force-revokes a file lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessRevokePayload {
    /// Token ID to revoke
    pub token_id: String,
    /// Revoking agent ID (main agent)
    pub revoker_id: String,
    /// Reason for revocation
    pub reason: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File conflict arbitrate: FileLockManager → Main Agent
///
/// Multiple agents conflict on same file, escalate to main agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConflictArbitratePayload {
    /// File path in conflict
    pub file_path: String,
    /// List of conflicting agent IDs
    pub conflicting_agents: Vec<String>,
    /// Access mode requested by each
    pub modes: Vec<String>,
    /// Reasons from each agent
    pub reasons: Vec<Option<String>>,
    /// Timeout for decision (seconds)
    pub timeout_secs: u64,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File ready notification: FileLockManager → Waiters
///
/// Broadcast when a WRITE completes (for READ_LATEST waiters).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadyPayload {
    /// File path that was written
    pub file_path: String,
    /// Agent that completed the write
    pub agent_id: String,
    /// Token ID that was released
    pub token_id: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// File error notification: FileLockManager → Waiters
///
/// Broadcast when a writer crashes (unblocks READ_LATEST waiters).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileErrorPayload {
    /// File path with error
    pub file_path: String,
    /// Agent that crashed
    pub agent_id: String,
    /// Error reason
    pub reason: String,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// System token issuance: System → Agent
///
/// System issues a system token for agent admission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTokenPayload {
    /// Token ID
    pub token_id: String,
    /// Agent ID
    pub agent_id: String,
    /// Session ID
    pub session_id: String,
    /// Project root
    pub project_root: String,
    /// Token expiration timestamp
    pub expires_at: u64,
    /// Heartbeat interval in seconds
    pub heartbeat_interval_secs: u64,
    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,
}

/// Outcome of a fanotify permission decision.
///
/// Published by the ergatai-lock enforcer on every `open()` decision so
/// downstream consumers (audit, UI) can observe kernel-level enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnforcementAction {
    /// The open was allowed (file not locked, caller is holder, or caller is
    /// a non-agent / allowlisted PID).
    Allowed,
    /// The open was denied because the file is locked by another agent.
    Denied,
    /// The enforcer encountered an error while deciding; access was allowed
    /// (fail-open) but the error is surfaced for diagnostics.
    Errored,
}

impl std::fmt::Display for EnforcementAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnforcementAction::Allowed => write!(f, "ALLOWED"),
            EnforcementAction::Denied => write!(f, "DENIED"),
            EnforcementAction::Errored => write!(f, "ERRORED"),
        }
    }
}

/// Kernel-level enforcement event: fanotify enforcer → observers.
///
/// Published on `ergatai.file.enforce.{project_id}` (FILE_EVENTS stream)
/// whenever the enforcer makes a decision. Consumers can use this to build
/// audit trails, dashboards, or trigger alerts on repeated denials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEnforcementPayload {
    /// File path (relative to project root, or absolute if relative resolution failed).
    pub file_path: String,
    /// PID of the process that called open().
    pub pid: u32,
    /// Resolved agent ID, if the PID belongs to a known agent.
    pub agent_id: Option<String>,
    /// Resolved session ID, if known.
    pub session_id: Option<String>,
    /// Decision outcome.
    pub action: EnforcementAction,
    /// Agent ID of the current WRITE lock holder (if denied).
    pub holder_agent_id: Option<String>,
    /// Session ID of the current WRITE lock holder (if denied).
    pub holder_session_id: Option<String>,
    /// Human-readable reason for the decision.
    pub reason: String,
    /// Timestamp (Unix epoch seconds).
    pub timestamp: u64,
}

/// Enum wrapping all event types — useful for a single subscriber
/// that wants to handle multiple event kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload")]
pub enum DagEvent {
    TaskSubmit(TaskSubmitPayload),
    NodeComplete(NodeCompletePayload),
    NodeFailed(NodeFailedPayload),
    DagComplete(DagCompletePayload),
    AgentMessage(AgentMessagePayload),
    // File access events
    FileAccessRequest(FileAccessRequestPayload),
    FileAccessGrant(FileAccessGrantPayload),
    FileAccessDeny(FileAccessDenyPayload),
    FileAccessEscalate(FileAccessEscalatePayload),
    FileAccessApprove(FileAccessApprovePayload),
    FileAccessReject(FileAccessRejectPayload),
    FileAccessRelease(FileAccessReleasePayload),
    FileAccessRevoke(FileAccessRevokePayload),
    FileConflictArbitrate(FileConflictArbitratePayload),
    FileReady(FileReadyPayload),
    FileError(FileErrorPayload),
    SystemToken(SystemTokenPayload),
    /// Kernel-level enforcement event (fanotify decisions).
    FileEnforcement(FileEnforcementPayload),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TaskSubmitPayload ──

    #[test]
    fn test_task_submit_roundtrip() {
        let payload = TaskSubmitPayload {
            task_id: "task-123".to_string(),
            plan_content: "# Task\nDo something".to_string(),
            plan_file: ".ergatai/.dag-plans/task-123.md".to_string(),
            target_agent: "claude-code".to_string(),
            priority: 2,
            timeout_secs: Some(300),
            dag_id: Some("dag-abc".to_string()),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: TaskSubmitPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.task_id, "task-123");
        assert_eq!(restored.plan_content, "# Task\nDo something");
        assert_eq!(restored.priority, 2);
        assert_eq!(restored.timeout_secs, Some(300));
        assert_eq!(restored.dag_id, Some("dag-abc".to_string()));
    }

    #[test]
    fn test_task_submit_optional_fields() {
        let payload = TaskSubmitPayload {
            task_id: "t1".to_string(),
            plan_content: "content".to_string(),
            plan_file: "path".to_string(),
            target_agent: "agent".to_string(),
            priority: 1,
            timeout_secs: None,
            dag_id: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: TaskSubmitPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.timeout_secs, None);
        assert_eq!(restored.dag_id, None);
    }

    // ── NodeCompletePayload ──

    #[test]
    fn test_node_complete_roundtrip() {
        let mut outputs = serde_json::Map::new();
        outputs.insert("review_result".to_string(), serde_json::Value::String("LGTM".to_string()));
        outputs.insert("issues".to_string(), serde_json::Value::String("3".to_string()));

        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "claude-code".to_string(),
            result_summary: Some("Done".to_string()),
            outputs: serde_json::Value::Object(outputs.clone()),
            result_file: Some(".ergatai/.dag-results/n1.md".to_string()),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: NodeCompletePayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.node_id, "n1");
        if let serde_json::Value::Object(ref obj) = restored.outputs {
            assert_eq!(obj.len(), 2);
            assert_eq!(
                obj.get("review_result"),
                Some(&serde_json::Value::String("LGTM".to_string()))
            );
        } else {
            panic!("Expected Object");
        }
        assert_eq!(
            restored.result_file,
            Some(".ergatai/.dag-results/n1.md".to_string())
        );
    }

    #[test]
    fn test_node_complete_empty_outputs() {
        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "agent".to_string(),
            result_summary: None,
            outputs: serde_json::Value::Object(serde_json::Map::new()),
            result_file: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: NodeCompletePayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored.outputs, serde_json::Value::Object(ref obj) if obj.is_empty()));
    }

    // ── NodeFailedPayload ──

    #[test]
    fn test_node_failed_roundtrip() {
        let payload = NodeFailedPayload {
            node_id: "n2".to_string(),
            task_id: "n2".to_string(),
            agent_name: "codex".to_string(),
            error: "timeout after 300s".to_string(),
            retryable: true,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: NodeFailedPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.node_id, "n2");
        assert_eq!(restored.error, "timeout after 300s");
        assert!(restored.retryable);
    }

    // ── DagCompletePayload ──

    #[test]
    fn test_dag_complete_roundtrip() {
        let payload = DagCompletePayload {
            dag_id: "dag-1".to_string(),
            total_nodes: 5,
            completed_nodes: 4,
            failed_nodes: 1,
            duration_secs: 120,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: DagCompletePayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.dag_id, "dag-1");
        assert_eq!(restored.total_nodes, 5);
        assert_eq!(restored.completed_nodes, 4);
        assert_eq!(restored.failed_nodes, 1);
        assert_eq!(restored.duration_secs, 120);
    }

    // ── DagEvent tagged enum ──

    #[test]
    fn test_dag_event_tagged_roundtrip() {
        let event = DagEvent::NodeComplete(NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "agent".to_string(),
            result_summary: Some("ok".to_string()),
            outputs: serde_json::Value::Object(serde_json::Map::new()),
            result_file: None,
        });

        let json = serde_json::to_string(&event).unwrap();
        // Verify it has the tag
        assert!(json.contains("\"event_type\":\"NodeComplete\""));

        let restored: DagEvent = serde_json::from_str(&json).unwrap();
        match restored {
            DagEvent::NodeComplete(p) => assert_eq!(p.node_id, "n1"),
            _ => panic!("Expected NodeComplete event"),
        }
    }

    #[test]
    fn test_dag_event_all_variants() {
        let events = vec![
            DagEvent::TaskSubmit(TaskSubmitPayload {
                task_id: "t".to_string(),
                plan_content: "c".to_string(),
                plan_file: "f".to_string(),
                target_agent: "a".to_string(),
                priority: 1,
                timeout_secs: None,
                dag_id: None,
            }),
            DagEvent::NodeFailed(NodeFailedPayload {
                node_id: "n".to_string(),
                task_id: "t".to_string(),
                agent_name: "a".to_string(),
                error: "err".to_string(),
                retryable: false,
            }),
            DagEvent::DagComplete(DagCompletePayload {
                dag_id: "d".to_string(),
                total_nodes: 3,
                completed_nodes: 3,
                failed_nodes: 0,
                duration_secs: 60,
            }),
            DagEvent::AgentMessage(AgentMessagePayload {
                from_agent: "claude-code".to_string(),
                to_agent: "codex".to_string(),
                from_uuid: None,
                to_uuid: None,
                content: "@codex please review this code".to_string(),
                thread_id: Some("thread-123".to_string()),
                timestamp: 1234567890,
                metadata: HashMap::new(),
            }),
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _: DagEvent = serde_json::from_str(&json).unwrap();
        }
    }

    // ── AgentMessagePayload ──

    #[test]
    fn test_agent_message_roundtrip() {
        let mut metadata = HashMap::new();
        metadata.insert("priority".to_string(), "high".to_string());

        let payload = AgentMessagePayload {
            from_agent: "claude-code".to_string(),
            to_agent: "codex".to_string(),
            from_uuid: Some("uuid-claude-123".to_string()),
            to_uuid: Some("uuid-codex-456".to_string()),
            content: "@codex please review this code".to_string(),
            thread_id: Some("thread-123".to_string()),
            timestamp: 1234567890,
            metadata: metadata.clone(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: AgentMessagePayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.from_agent, "claude-code");
        assert_eq!(restored.to_agent, "codex");
        assert_eq!(restored.content, "@codex please review this code");
        assert_eq!(restored.thread_id, Some("thread-123".to_string()));
        assert_eq!(restored.timestamp, 1234567890);
        assert_eq!(restored.metadata.get("priority"), Some(&"high".to_string()));
    }

    #[test]
    fn test_agent_message_optional_fields() {
        let payload = AgentMessagePayload {
            from_agent: "agent-a".to_string(),
            to_agent: "agent-b".to_string(),
            from_uuid: None,
            to_uuid: None,
            content: "hello".to_string(),
            thread_id: None,
            timestamp: 0,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: AgentMessagePayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.thread_id, None);
        assert!(restored.metadata.is_empty());
    }

    // ── FileAccessRequestPayload ──

    #[test]
    fn test_file_access_request_roundtrip() {
        let payload = FileAccessRequestPayload {
            request_id: "req-123".to_string(),
            agent_id: "agent-a".to_string(),
            session_id: "sess-456".to_string(),
            file_path: "src/main.rs".to_string(),
            mode: "WRITE".to_string(),
            reason: Some("Need to update code".to_string()),
            node_id: Some("node-1".to_string()),
            expected_duration_secs: Some(60),
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: FileAccessRequestPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.request_id, "req-123");
        assert_eq!(restored.agent_id, "agent-a");
        assert_eq!(restored.file_path, "src/main.rs");
        assert_eq!(restored.mode, "WRITE");
        assert_eq!(restored.reason, Some("Need to update code".to_string()));
        assert_eq!(restored.timestamp, 1234567890);
    }

    // ── FileAccessGrantPayload ──

    #[test]
    fn test_file_access_grant_roundtrip() {
        let payload = FileAccessGrantPayload {
            request_id: "req-123".to_string(),
            token_id: "token-789".to_string(),
            agent_id: "agent-b".to_string(),
            file_path: "src/lib.rs".to_string(),
            mode: "READ".to_string(),
            approved_by: "system".to_string(),
            expires_at: 1234567999,
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: FileAccessGrantPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.token_id, "token-789");
        assert_eq!(restored.approved_by, "system");
        assert_eq!(restored.expires_at, 1234567999);
    }

    // ── FileReadyPayload ──

    #[test]
    fn test_file_ready_roundtrip() {
        let payload = FileReadyPayload {
            file_path: "output.txt".to_string(),
            agent_id: "writer-agent".to_string(),
            token_id: "tok-123".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: FileReadyPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.file_path, "output.txt");
        assert_eq!(restored.agent_id, "writer-agent");
    }

    // ── SystemTokenPayload ──

    #[test]
    fn test_system_token_roundtrip() {
        let payload = SystemTokenPayload {
            token_id: "sys-tok-1".to_string(),
            agent_id: "agent-x".to_string(),
            session_id: "sess-999".to_string(),
            project_root: "/home/user/project".to_string(),
            expires_at: 1234569999,
            heartbeat_interval_secs: 30,
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: SystemTokenPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.token_id, "sys-tok-1");
        assert_eq!(restored.heartbeat_interval_secs, 30);
        assert_eq!(restored.project_root, "/home/user/project");
    }

    // ── Edge cases ──

    #[test]
    fn test_agent_message_empty_content() {
        let payload = AgentMessagePayload {
            from_agent: "a".to_string(),
            to_agent: "b".to_string(),
            from_uuid: None,
            to_uuid: None,
            content: "".to_string(),
            thread_id: None,
            timestamp: 0,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: AgentMessagePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.content, "");
    }

    #[test]
    fn test_agent_message_long_content() {
        let long_content = "x".repeat(10000);
        let payload = AgentMessagePayload {
            from_agent: "a".to_string(),
            to_agent: "b".to_string(),
            from_uuid: None,
            to_uuid: None,
            content: long_content.clone(),
            thread_id: None,
            timestamp: 0,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: AgentMessagePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.content.len(), 10000);
        assert_eq!(restored.content, long_content);
    }

    #[test]
    fn test_agent_message_special_chars() {
        let payload = AgentMessagePayload {
            from_agent: "agent-1".to_string(),
            to_agent: "agent-2".to_string(),
            from_uuid: None,
            to_uuid: None,
            content: "Hello\nWorld\t\"quotes\" and \\slashes\\".to_string(),
            thread_id: None,
            timestamp: 0,
            metadata: HashMap::new(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: AgentMessagePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.content, "Hello\nWorld\t\"quotes\" and \\slashes\\");
    }

    #[test]
    fn test_dag_event_file_access_variants() {
        let events = vec![
            DagEvent::FileAccessRequest(FileAccessRequestPayload {
                request_id: "r1".to_string(),
                agent_id: "a1".to_string(),
                session_id: "s1".to_string(),
                file_path: "f1".to_string(),
                mode: "READ".to_string(),
                reason: None,
                node_id: None,
                expected_duration_secs: None,
                timestamp: 100,
            }),
            DagEvent::FileReady(FileReadyPayload {
                file_path: "f1".to_string(),
                agent_id: "a1".to_string(),
                token_id: "t1".to_string(),
                timestamp: 200,
            }),
            DagEvent::SystemToken(SystemTokenPayload {
                token_id: "t1".to_string(),
                agent_id: "a1".to_string(),
                session_id: "s1".to_string(),
                project_root: "/p".to_string(),
                expires_at: 300,
                heartbeat_interval_secs: 30,
                timestamp: 100,
            }),
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _: DagEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_file_access_optional_fields() {
        let payload = FileAccessRequestPayload {
            request_id: "req-1".to_string(),
            agent_id: "agent-1".to_string(),
            session_id: "sess-1".to_string(),
            file_path: "file.txt".to_string(),
            mode: "READ".to_string(),
            reason: None,
            node_id: None,
            expected_duration_secs: None,
            timestamp: 100,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: FileAccessRequestPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.reason, None);
        assert_eq!(restored.node_id, None);
        assert_eq!(restored.expected_duration_secs, None);
    }
}
