//! JetStream stream definition for reliable agent-to-agent messaging
//!
//! Ensures agent messages are persisted before delivery, providing:
//! - **Durability**: Messages survive process crashes and restarts
//! - **Retry on failure**: Unacked messages are redelivered automatically
//! - **Back-pressure**: WorkQueue retention prevents unbounded growth
//!
//! ## Flow
//!
//! ```text
//! send_message MCP tool
//!   ↓ publish to JetStream (AGENT_MESSAGES)
//!   ↓ return "queued" to caller immediately
//!   ↓
//! MessageDeliveryConsumer (background task)
//!   ↓ pull from stream
//!   ↓ try tmux injection → ack on success
//!   ↓ fallback: MCP notification → ack on success
//!   ↓ both fail → nak (redeliver after ack_wait timeout)
//! ```

use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType};
use std::time::Duration;

/// JetStream stream name for agent-to-agent messages
pub const AGENT_MESSAGES_STREAM: &str = "AGENT_MESSAGES";

/// Create JetStream stream configuration for agent messages
///
/// This stream persists agent-to-agent messages so they survive:
/// - NATS server restarts (file-backed storage)
/// - Consumer crashes (unacked messages redeliver after `ack_wait`)
/// - Slow consumers (WorkQueue retention allows batch processing)
///
/// Subject pattern: `ergatai.agent.message.*`
/// - Each agent gets a per-id subject: `ergatai.agent.message.{agent_id}`
/// - The wildcard lets one consumer pull all messages for all agents
///
/// # Reliability guarantees
///
/// | Setting | Value | Rationale |
/// |---------|-------|-----------|
/// | `retention` | WorkQueue | Message deleted only after ack — prevents loss |
/// | `max_age` | 24h | Stale messages expire (agent offline >24h is abnormal) |
/// | `storage` | File | Survives process restart |
///
/// Note: `max_deliver` and `ack_wait` are set on the **consumer** (pull side),
/// not on the stream. See `MessageDeliveryConsumer` for those settings.
pub fn agent_message_stream_config() -> Config {
    Config {
        name: AGENT_MESSAGES_STREAM.to_string(),
        subjects: vec!["ergatai.agent.message.*".to_string()],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(86_400), // 24 hours
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Return all agent-messaging stream configs for initialization at startup.
pub fn all_agent_message_stream_configs() -> Vec<Config> {
    vec![agent_message_stream_config()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_message_stream_config() {
        let config = agent_message_stream_config();
        assert_eq!(config.name, AGENT_MESSAGES_STREAM);
        assert_eq!(config.subjects, vec!["ergatai.agent.message.*"]);
        assert_eq!(config.retention, RetentionPolicy::WorkQueue);
        assert_eq!(config.max_age, Duration::from_secs(86_400));
        assert_eq!(config.storage, StorageType::File);
    }

    #[test]
    fn test_all_agent_message_stream_configs() {
        let configs = all_agent_message_stream_configs();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, AGENT_MESSAGES_STREAM);
    }

    #[test]
    fn test_agent_message_stream_storage_type() {
        let config = agent_message_stream_config();
        assert_eq!(config.storage, StorageType::File);
    }

    #[test]
    fn test_agent_message_stream_num_replicas() {
        let config = agent_message_stream_config();
        assert_eq!(config.num_replicas, 1);
    }

    #[test]
    fn test_agent_message_stream_subject_wildcard() {
        let config = agent_message_stream_config();
        // Subject should use wildcard to match all agent message subjects
        assert!(config.subjects[0].contains('*'), "Subject should contain wildcard");
        assert_eq!(config.subjects[0], "ergatai.agent.message.*");
    }
}
