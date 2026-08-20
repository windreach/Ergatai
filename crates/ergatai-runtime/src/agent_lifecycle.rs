//! Agent Lifecycle State Machine
//!
//! This module defines a unified state machine for agent lifecycle management,
//! consolidating three separate state systems into a single, compile-time safe
//! state machine using the `state-machines` crate.
//!
//! # States
//!
//! The agent lifecycle consists of 15 states covering all phases of agent execution:
//!
//! - **Created**: Agent created but not yet initialized
//! - **Initializing**: Loading context, configuration, and dependencies
//! - **WaitingForResources**: Waiting for locks, concurrency slots, or dependencies
//! - **Idle**: Ready to accept work but has no assigned task
//! - **Starting**: Agent process starting (workspace created, process spawning)
//! - **Running**: Agent running and ready to accept tasks
//! - **Processing**: Processing a specific message or task
//! - **Stopping**: Agent stopping gracefully (shutdown in progress)
//! - **Terminated**: Agent terminated successfully
//! - **Failed**: Agent failed with error
//! - **TimedOut**: Agent timed out (heartbeat missed or max runtime exceeded)
//! - **Signaled**: Agent killed by signal
//! - **Paused**: Agent paused (suspended execution)
//! - **Reconnecting**: Agent reconnecting after disconnect
//! - **Maintenance**: Agent in maintenance mode (manual intervention required)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Processing phase within Running state
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessingPhase {
    /// Planning the task approach
    Planning,
    /// Reading files and gathering context
    Reading,
    /// Writing or modifying code
    Writing,
    /// Running tests
    Testing,
    /// Reviewing code or results
    Reviewing,
    /// Custom phase (user-defined)
    Custom(String),
}

/// Resource type being waited for
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceType {
    /// Waiting for file lock
    FileLock,
    /// Waiting for concurrency slot
    ConcurrencySlot,
    /// Waiting for dependency task completion
    DependencyTask,
    /// Waiting for token (file access token)
    Token,
    /// Waiting for network resource
    NetworkResource,
}

/// Reason for stopping
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// User requested stop
    UserRequested,
    /// Task completed successfully
    TaskCompleted,
    /// System shutdown
    Shutdown,
    /// Preempted by higher priority task
    Preempted,
    /// Error occurred
    Error,
}

/// Timeout type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeoutType {
    /// Heartbeat timeout (agent not responding)
    Heartbeat,
    /// Maximum runtime exceeded
    MaxRuntime,
    /// Idle timeout (no work for too long)
    IdleTimeout,
    /// Task-specific timeout
    TaskTimeout,
}

/// Agent lifecycle state machine.
///
/// This enum defines all possible states an agent can be in during its lifecycle.
/// State transitions are managed through the `transition_to` method which validates
/// allowed transitions at runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentLifecycleState {
    /// Agent created but not yet initialized
    ///
    /// Initial state when an agent record is created but no initialization has started.
    Created,

    /// Loading context, configuration, and dependencies
    ///
    /// Agent is loading its context (AGENT.md, task plan, configuration files)
    /// and setting up dependencies.
    Initializing {
        /// When initialization started
        started_at: DateTime<Utc>,
        /// Source of context (e.g., file path, URL)
        context_source: Option<String>,
    },

    /// Waiting for resources (locks, concurrency slots, dependencies)
    ///
    /// Agent is blocked waiting for a resource to become available.
    WaitingForResources {
        /// Type of resource being waited for
        resource_type: ResourceType,
        /// When the wait started
        waiting_since: DateTime<Utc>,
        /// Human-readable reason for the wait
        reason: String,
    },

    /// Ready to accept work but idle
    ///
    /// Agent has finished initialization and is ready to accept tasks,
    /// but currently has no assigned work.
    Idle {
        /// When the agent became idle
        ready_since: DateTime<Utc>,
        /// Agent capabilities (tools it can use)
        capabilities: Vec<String>,
    },

    /// Agent process starting (workspace created, process spawning)
    ///
    /// Agent workspace has been created and the process is being spawned.
    Starting {
        /// Workspace ID
        workspace_id: String,
        /// Command being executed
        command: String,
        /// When the start was initiated
        started_at: DateTime<Utc>,
    },

    /// Agent running and ready to accept tasks
    ///
    /// Agent is running and can accept tasks. This is the general "active" state.
    Running {
        /// Currently assigned task ID (if any)
        task_id: Option<String>,
        /// When the agent started running
        started_at: DateTime<Utc>,
        /// Last heartbeat timestamp (for hang detection)
        last_heartbeat: DateTime<Utc>,
    },

    /// Processing a specific message or task
    ///
    /// Agent is actively processing a task or message. This is a more specific
    /// state than Running, indicating the agent is not just idle but actively working.
    Processing {
        /// Task ID being processed
        task_id: String,
        /// Current phase of processing
        phase: ProcessingPhase,
        /// When processing started
        started_at: DateTime<Utc>,
    },

    /// Agent stopping gracefully (shutdown in progress)
    ///
    /// Agent has been instructed to stop and is performing graceful shutdown.
    Stopping {
        /// Reason for stopping
        reason: StopReason,
        /// When the stop was initiated
        initiated_at: DateTime<Utc>,
        /// Timeout for graceful shutdown (seconds)
        timeout_secs: Option<u64>,
    },

    /// Agent terminated successfully
    ///
    /// Agent has exited normally (terminal state).
    Terminated {
        /// Exit code (0 = success)
        exit_code: Option<i32>,
        /// When the agent terminated
        terminated_at: DateTime<Utc>,
        /// Total runtime duration (seconds)
        duration_secs: u64,
    },

    /// Agent failed with error
    ///
    /// Agent encountered an error and terminated abnormally (terminal state).
    Failed {
        /// Error message
        error: String,
        /// Whether the failure is retryable
        retryable: bool,
        /// When the failure occurred
        failed_at: DateTime<Utc>,
    },

    /// Agent timed out (heartbeat missed or max runtime exceeded)
    ///
    /// Agent stopped responding or exceeded its maximum runtime (terminal state).
    TimedOut {
        /// Type of timeout
        timeout_type: TimeoutType,
        /// When the timeout was detected
        detected_at: DateTime<Utc>,
        /// Last heartbeat before timeout
        last_heartbeat: DateTime<Utc>,
    },

    /// Agent killed by signal
    ///
    /// Agent was terminated by a signal (SIGTERM, SIGKILL, etc.) (terminal state).
    Signaled {
        /// Signal number (e.g., 9 = SIGKILL, 15 = SIGTERM)
        signal: i32,
        /// When the signal was received
        signaled_at: DateTime<Utc>,
    },

    /// Agent paused (suspended execution)
    ///
    /// Agent execution has been temporarily suspended.
    Paused {
        /// When the agent was paused
        paused_at: DateTime<Utc>,
        /// Reason for pausing
        reason: Option<String>,
    },

    /// Agent reconnecting after disconnect
    ///
    /// Agent lost connection and is attempting to reconnect.
    Reconnecting {
        /// Reconnection attempt number
        attempt: u32,
        /// When reconnection started
        reconnecting_since: DateTime<Utc>,
    },

    /// Agent in maintenance mode (manual intervention required)
    ///
    /// Agent requires manual intervention or is undergoing maintenance.
    Maintenance {
        /// When maintenance mode was entered
        entered_at: DateTime<Utc>,
        /// Reason for maintenance
        reason: String,
    },
}

impl AgentLifecycleState {
    /// Check if the agent is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentLifecycleState::Terminated { .. }
                | AgentLifecycleState::Failed { .. }
                | AgentLifecycleState::TimedOut { .. }
                | AgentLifecycleState::Signaled { .. }
        )
    }

    /// Check if the agent is alive (not in a terminal state)
    pub fn is_alive(&self) -> bool {
        !self.is_terminal()
    }

    /// Check if the agent is idle (ready for work)
    pub fn is_idle(&self) -> bool {
        matches!(self, AgentLifecycleState::Idle { .. })
    }

    /// Check if the agent is processing a task
    pub fn is_processing(&self) -> bool {
        matches!(self, AgentLifecycleState::Processing { .. })
    }

    /// Check if the agent is waiting for resources
    pub fn is_waiting(&self) -> bool {
        matches!(self, AgentLifecycleState::WaitingForResources { .. })
    }

    /// Get the task ID if the agent is processing a task
    pub fn task_id(&self) -> Option<&str> {
        match self {
            AgentLifecycleState::Running { task_id, .. } => task_id.as_deref(),
            AgentLifecycleState::Processing { task_id, .. } => Some(task_id),
            _ => None,
        }
    }

    /// Get the last heartbeat timestamp if available
    pub fn last_heartbeat(&self) -> Option<DateTime<Utc>> {
        match self {
            AgentLifecycleState::Running { last_heartbeat, .. } => Some(*last_heartbeat),
            AgentLifecycleState::Processing { .. } => None, // Could be extended to track heartbeat
            AgentLifecycleState::TimedOut { last_heartbeat, .. } => Some(*last_heartbeat),
            _ => None,
        }
    }

    /// Convert to the legacy `AgentState` enum for backward compatibility.
    ///
    /// This is a lossy mapping from the 15-state lifecycle to the 5-state legacy enum.
    /// Used only to keep the deprecated `state` field in sync with `lifecycle`.
    #[allow(deprecated)]
    pub fn to_legacy_state(&self) -> crate::types::AgentState {
        use crate::types::AgentState;
        match self {
            AgentLifecycleState::Created => AgentState::Starting,
            AgentLifecycleState::Initializing { .. } => AgentState::Starting,
            AgentLifecycleState::WaitingForResources { .. } => AgentState::Starting,
            AgentLifecycleState::Idle { .. } => AgentState::Running,
            AgentLifecycleState::Starting { .. } => AgentState::Starting,
            AgentLifecycleState::Running { .. } => AgentState::Running,
            AgentLifecycleState::Processing { .. } => AgentState::Running,
            AgentLifecycleState::Stopping { .. } => AgentState::Stopping,
            AgentLifecycleState::Terminated { .. } => AgentState::Stopped,
            AgentLifecycleState::Failed { error, .. } => AgentState::Failed(error.clone()),
            AgentLifecycleState::TimedOut { .. } => AgentState::Stopped,
            AgentLifecycleState::Signaled { .. } => AgentState::Stopped,
            AgentLifecycleState::Paused { .. } => AgentState::Stopped,
            AgentLifecycleState::Reconnecting { .. } => AgentState::Starting,
            AgentLifecycleState::Maintenance { .. } => AgentState::Running,
        }
    }

    /// Get a human-readable state name
    pub fn state_name(&self) -> &'static str {
        match self {
            AgentLifecycleState::Created => "created",
            AgentLifecycleState::Initializing { .. } => "initializing",
            AgentLifecycleState::WaitingForResources { .. } => "waiting_for_resources",
            AgentLifecycleState::Idle { .. } => "idle",
            AgentLifecycleState::Starting { .. } => "starting",
            AgentLifecycleState::Running { .. } => "running",
            AgentLifecycleState::Processing { .. } => "processing",
            AgentLifecycleState::Stopping { .. } => "stopping",
            AgentLifecycleState::Terminated { .. } => "terminated",
            AgentLifecycleState::Failed { .. } => "failed",
            AgentLifecycleState::TimedOut { .. } => "timed_out",
            AgentLifecycleState::Signaled { .. } => "signaled",
            AgentLifecycleState::Paused { .. } => "paused",
            AgentLifecycleState::Reconnecting { .. } => "reconnecting",
            AgentLifecycleState::Maintenance { .. } => "maintenance",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_name() {
        assert_eq!(AgentLifecycleState::Created.state_name(), "created");
        assert_eq!(
            AgentLifecycleState::Initializing {
                started_at: Utc::now(),
                context_source: None
            }
            .state_name(),
            "initializing"
        );
        assert_eq!(
            AgentLifecycleState::Running {
                task_id: None,
                started_at: Utc::now(),
                last_heartbeat: Utc::now()
            }
            .state_name(),
            "running"
        );
    }

    #[test]
    fn test_is_terminal() {
        assert!(AgentLifecycleState::Terminated {
            exit_code: Some(0),
            terminated_at: Utc::now(),
            duration_secs: 100
        }
        .is_terminal());

        assert!(AgentLifecycleState::Failed {
            error: "test".to_string(),
            retryable: false,
            failed_at: Utc::now()
        }
        .is_terminal());

        assert!(!AgentLifecycleState::Running {
            task_id: None,
            started_at: Utc::now(),
            last_heartbeat: Utc::now()
        }
        .is_terminal());
    }

    #[test]
    fn test_is_alive() {
        assert!(AgentLifecycleState::Running {
            task_id: None,
            started_at: Utc::now(),
            last_heartbeat: Utc::now()
        }
        .is_alive());

        assert!(!AgentLifecycleState::Terminated {
            exit_code: Some(0),
            terminated_at: Utc::now(),
            duration_secs: 100
        }
        .is_alive());
    }

    #[test]
    fn test_task_id() {
        let running = AgentLifecycleState::Running {
            task_id: Some("task-123".to_string()),
            started_at: Utc::now(),
            last_heartbeat: Utc::now(),
        };
        assert_eq!(running.task_id(), Some("task-123"));

        let processing = AgentLifecycleState::Processing {
            task_id: "task-456".to_string(),
            phase: ProcessingPhase::Planning,
            started_at: Utc::now(),
        };
        assert_eq!(processing.task_id(), Some("task-456"));

        let idle = AgentLifecycleState::Idle {
            ready_since: Utc::now(),
            capabilities: vec![],
        };
        assert_eq!(idle.task_id(), None);
    }

    #[test]
    fn test_last_heartbeat() {
        let now = Utc::now();
        let running = AgentLifecycleState::Running {
            task_id: None,
            started_at: now,
            last_heartbeat: now,
        };
        assert_eq!(running.last_heartbeat(), Some(now));

        let idle = AgentLifecycleState::Idle {
            ready_since: now,
            capabilities: vec![],
        };
        assert_eq!(idle.last_heartbeat(), None);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let state = AgentLifecycleState::Running {
            task_id: Some("task-123".to_string()),
            started_at: Utc::now(),
            last_heartbeat: Utc::now(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: AgentLifecycleState = serde_json::from_str(&json).unwrap();

        assert_eq!(state, decoded);
    }

    #[test]
    fn test_processing_phases() {
        let phases = vec![
            ProcessingPhase::Planning,
            ProcessingPhase::Reading,
            ProcessingPhase::Writing,
            ProcessingPhase::Testing,
            ProcessingPhase::Reviewing,
            ProcessingPhase::Custom("custom_phase".to_string()),
        ];

        for phase in phases {
            let json = serde_json::to_string(&phase).unwrap();
            let decoded: ProcessingPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(phase, decoded);
        }
    }

    #[test]
    fn test_resource_types() {
        let types = vec![
            ResourceType::FileLock,
            ResourceType::ConcurrencySlot,
            ResourceType::DependencyTask,
            ResourceType::Token,
            ResourceType::NetworkResource,
        ];

        for resource_type in types {
            let json = serde_json::to_string(&resource_type).unwrap();
            let decoded: ResourceType = serde_json::from_str(&json).unwrap();
            assert_eq!(resource_type, decoded);
        }
    }

    #[test]
    fn test_stop_reasons() {
        let reasons = vec![
            StopReason::UserRequested,
            StopReason::TaskCompleted,
            StopReason::Shutdown,
            StopReason::Preempted,
            StopReason::Error,
        ];

        for reason in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            let decoded: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(reason, decoded);
        }
    }

    #[test]
    fn test_timeout_types() {
        let types = vec![
            TimeoutType::Heartbeat,
            TimeoutType::MaxRuntime,
            TimeoutType::IdleTimeout,
            TimeoutType::TaskTimeout,
        ];

        for timeout_type in types {
            let json = serde_json::to_string(&timeout_type).unwrap();
            let decoded: TimeoutType = serde_json::from_str(&json).unwrap();
            assert_eq!(timeout_type, decoded);
        }
    }
}
