// Cross-Agent Communication Module
// File-based collaboration system for multi-agent coordination

pub mod acp_bridge;
pub mod task_coordinator;
pub mod plan_watcher;
pub mod agent_launcher;
pub mod task_scheduler;
pub mod dag_scheduler;   // DAG-based scheduler

pub use acp_bridge::detect_cross_agent_intent;
pub use task_coordinator::TaskCoordinator;
pub use plan_watcher::PollingWatcher;
pub use agent_launcher::{AgentLauncher, RunningAgent, AgentStatus};
pub use task_scheduler::{TaskScheduler, ScheduleStrategy, AgentAvailability, PendingTask, global_scheduler};
pub use dag_scheduler::{DagScheduler, set_dag_scheduler, get_dag_scheduler, clear_dag_scheduler};
