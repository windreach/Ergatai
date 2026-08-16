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

/// Initialize a pull consumer on the DAG_EVENTS stream with the given name and filter.
///
/// Shared helper used by both TaskScheduler (task submissions) and DagScheduler (dag events)
/// to avoid duplicating consumer setup logic.
pub async fn init_dag_stream_pull_consumer(
    connection: &crate::NatsConnection,
    consumer_name: &str,
    filter_subject: &str,
) -> Result<
    futures_util::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
    String,
> {
    use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
    use futures_util::StreamExt;

    let stream = connection
        .jetstream()
        .get_stream(DAG_EVENTS_STREAM)
        .await
        .map_err(|e| format!("Stream {} not found: {}", DAG_EVENTS_STREAM, e))?;

    let consumer_config = pull::Config {
        durable_name: Some(consumer_name.to_string()),
        filter_subject: filter_subject.to_string(),
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(60),
        max_deliver: 5,
        ..Default::default()
    };

    let consumer = stream
        .get_or_create_consumer(consumer_name, consumer_config)
        .await
        .map_err(|e| format!("Failed to create consumer '{}': {}", consumer_name, e))?;

    let messages = consumer
        .messages()
        .await
        .map_err(|e| format!("Failed to get message stream: {}", e))?;

    Ok(Box::pin(messages.map(|r| {
        r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    })))
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

    #[test]
    fn test_dag_events_stream_constants() {
        assert_eq!(DAG_EVENTS_STREAM, "DAG_EVENTS");
        assert_eq!(TASK_SUBMISSIONS_CONSUMER, "task_submissions");
        assert_eq!(DAG_EVENTS_CONSUMER, "dag_events");
    }

    #[test]
    fn test_dag_events_stream_storage_and_replicas() {
        let config = dag_events_stream_config();
        assert_eq!(config.storage, StorageType::File);
        assert_eq!(config.num_replicas, 1);
    }

    #[test]
    fn test_dag_events_stream_subjects() {
        let config = dag_events_stream_config();
        // Should have exactly 2 subjects: task submit and dag events
        assert_eq!(config.subjects.len(), 2);
        // First subject uses single-level wildcard
        assert!(config.subjects[0].ends_with("*"));
        // Second subject uses multi-level wildcard
        assert!(config.subjects[1].ends_with(">"));
    }
}
