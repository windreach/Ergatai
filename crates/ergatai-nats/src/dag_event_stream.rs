//! JetStream stream definition for DAG orchestration events
//!
//! Ensures DAG task submissions, node completions, node failures, and DAG
//! completions are persisted before processing. This prevents:
//! - **Lost task submissions** if TaskScheduler crashes mid-startup
//! - **Lost completion events** if DagScheduler is restarting when agents finish
//! - **Lost DAG completion notifications** if observers are not yet connected
//!
//! ## Subjects covered
//!
//! | Subject | Publisher | Subscriber |
//! |---------|-----------|------------|
//! | `ergatai.task.submit.*` | DagScheduler | TaskScheduler |
//! | `ergatai.dag.node_complete.*` | AgentLauncher | DagScheduler |
//! | `ergatai.dag.node_failed.*` | AgentLauncher | DagScheduler |
//! | `ergatai.dag.complete.*` | DagScheduler | Observers |
//!
//! ## Consumer topology
//!
//! One stream, two pull consumers:
//! - `task_submissions` — filter `ergatai.task.submit.*` → TaskScheduler
//! - `dag_events` — filter `ergatai.dag.>` → DagScheduler
//!
//! WorkQueue retention ensures each message is deleted after ack.

use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType};
use std::time::Duration;

/// JetStream stream name for DAG orchestration events
pub const DAG_EVENTS_STREAM: &str = "DAG_EVENTS";

/// Pull consumer name for task submissions (used by TaskScheduler)
pub const TASK_SUBMISSIONS_CONSUMER: &str = "task_submissions";

/// Pull consumer name for DAG events (used by DagScheduler)
pub const DAG_EVENTS_CONSUMER: &str = "dag_events";

/// Create JetStream stream configuration for DAG events
///
/// Covers both task submission and DAG lifecycle subjects.
/// The two subscriber groups (TaskScheduler, DagScheduler) use filtered
/// pull consumers on this single stream.
pub fn dag_events_stream_config() -> Config {
    Config {
        name: DAG_EVENTS_STREAM.to_string(),
        subjects: vec![
            "ergatai.task.submit.*".to_string(),
            "ergatai.dag.>".to_string(),
        ],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(86_400), // 24 hours
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Return all DAG event stream configs for initialization at startup.
pub fn all_dag_event_stream_configs() -> Vec<Config> {
    vec![dag_events_stream_config()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dag_events_stream_config() {
        let config = dag_events_stream_config();
        assert_eq!(config.name, DAG_EVENTS_STREAM);
        assert_eq!(config.subjects.len(), 2);
        assert!(config
            .subjects
            .contains(&"ergatai.task.submit.*".to_string()));
        assert!(config.subjects.contains(&"ergatai.dag.>".to_string()));
        assert_eq!(config.retention, RetentionPolicy::WorkQueue);
        assert_eq!(config.max_age, Duration::from_secs(86_400));
        assert_eq!(config.storage, StorageType::File);
    }

    #[test]
    fn test_all_dag_event_stream_configs() {
        let configs = all_dag_event_stream_configs();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, DAG_EVENTS_STREAM);
    }
}
