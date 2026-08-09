//! NATS event payload types for DAG event-driven communication
//!
//! Defines the message payloads published/subscribed via NATS subjects.
//! All payloads are wrapped in `TaskMessage<T>` (from `task_queue.rs`)
//! which adds message_id, correlation_id, timestamp, retry metadata.
//!
//! ## Subject mapping
//!
//! | Payload               | Subject                                    | Publisher       | Subscriber    |
//! |-----------------------|--------------------------------------------|-----------------|---------------|
//! | `TaskSubmitPayload`   | `ergatai.task.submit.{target_agent}`       | DagScheduler    | TaskScheduler |
//! | `NodeCompletePayload` | `ergatai.dag.node_complete.{node_id}`      | AgentLauncher   | DagScheduler  |
//! | `NodeFailedPayload`   | `ergatai.dag.node_failed.{node_id}`       | AgentLauncher   | DagScheduler  |
//! | `DagCompletePayload`  | `ergatai.dag.complete.{dag_id}`            | DagScheduler    | Observers     |

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
    /// Structured key-value outputs → fed into DagContext for template rendering
    pub outputs: HashMap<String, String>,
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

/// Enum wrapping all event types — useful for a single subscriber
/// that wants to handle multiple event kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload")]
pub enum DagEvent {
    TaskSubmit(TaskSubmitPayload),
    NodeComplete(NodeCompletePayload),
    NodeFailed(NodeFailedPayload),
    DagComplete(DagCompletePayload),
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
        let mut outputs = HashMap::new();
        outputs.insert("review_result".to_string(), "LGTM".to_string());
        outputs.insert("issues".to_string(), "3".to_string());

        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "claude-code".to_string(),
            result_summary: Some("Done".to_string()),
            outputs: outputs.clone(),
            result_file: Some(".ergatai/.dag-results/n1.md".to_string()),
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: NodeCompletePayload = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.node_id, "n1");
        assert_eq!(restored.outputs.len(), 2);
        assert_eq!(restored.outputs.get("review_result"), Some(&"LGTM".to_string()));
        assert_eq!(restored.result_file, Some(".ergatai/.dag-results/n1.md".to_string()));
    }

    #[test]
    fn test_node_complete_empty_outputs() {
        let payload = NodeCompletePayload {
            node_id: "n1".to_string(),
            task_id: "n1".to_string(),
            agent_name: "agent".to_string(),
            result_summary: None,
            outputs: HashMap::new(),
            result_file: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        let restored: NodeCompletePayload = serde_json::from_str(&json).unwrap();
        assert!(restored.outputs.is_empty());
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
            outputs: HashMap::new(),
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
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _: DagEvent = serde_json::from_str(&json).unwrap();
        }
    }
}
