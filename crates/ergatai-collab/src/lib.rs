// Cross-Agent Communication Module
// File-based collaboration system for multi-agent coordination

pub mod agent_launcher;
pub mod dag_scheduler; // DAG-based scheduler
pub mod message_router;
pub mod plan_watcher;
pub mod task_coordinator;
pub mod task_scheduler; // Agent-to-agent message routing via NATS
pub mod tmux; // Tmux-based agent management

pub use agent_launcher::{AgentLauncher, AgentStatus, RunningAgent};
pub use dag_scheduler::{clear_dag_scheduler, get_dag_scheduler, set_dag_scheduler, DagScheduler};
pub use message_router::{extract_mentions, route_agent_message, scan_and_route_mentions};
pub use plan_watcher::PollingWatcher;
pub use task_coordinator::TaskCoordinator;
pub use task_scheduler::{
    global_scheduler, AgentAvailability, PendingTask, ScheduleStrategy, TaskScheduler,
};
pub use tmux::TmuxManager;
