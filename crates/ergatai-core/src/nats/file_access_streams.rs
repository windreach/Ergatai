//! JetStream stream definitions for file access control
//!
//! Defines JetStream streams for critical file access subjects to ensure
//! message persistence and reliability.

use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType};
use std::time::Duration;

/// JetStream stream name for file access requests
pub const FILE_ACCESS_REQUEST_STREAM: &str = "FILE_ACCESS_REQUESTS";

/// JetStream stream name for file access grants
pub const FILE_ACCESS_GRANT_STREAM: &str = "FILE_ACCESS_GRANTS";

/// JetStream stream name for file access escalations
pub const FILE_ACCESS_ESCALATE_STREAM: &str = "FILE_ACCESS_ESCALATIONS";

/// JetStream stream name for file events (ready/error)
pub const FILE_EVENTS_STREAM: &str = "FILE_EVENTS";

/// JetStream stream name for lock waiters queue
pub const LOCK_WAITERS_STREAM: &str = "LOCK_WAITERS";

/// Create JetStream stream configuration for file access requests
///
/// This stream persists file access requests to ensure they are not lost
/// even if the FileLockManager is temporarily unavailable.
pub fn file_access_request_stream_config() -> Config {
    Config {
        name: FILE_ACCESS_REQUEST_STREAM.to_string(),
        subjects: vec!["ergatai.file.access.request".to_string()],
        retention: RetentionPolicy::WorkQueue, // Auto-delete after ack
        max_age: Duration::from_secs(3600),    // 1 hour
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Create JetStream stream configuration for file access grants
///
/// This stream persists file access grants to ensure agents receive
/// their tokens even if they temporarily disconnect.
pub fn file_access_grant_stream_config() -> Config {
    Config {
        name: FILE_ACCESS_GRANT_STREAM.to_string(),
        subjects: vec!["ergatai.file.access.grant.*".to_string()],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(3600),
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Create JetStream stream configuration for file access escalations
///
/// This stream persists escalation requests to main agents to ensure
/// approval decisions are not lost.
pub fn file_access_escalate_stream_config() -> Config {
    Config {
        name: FILE_ACCESS_ESCALATE_STREAM.to_string(),
        subjects: vec!["ergatai.file.access.escalate.*".to_string()],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(1800), // 30 minutes (approval timeout)
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Create JetStream stream configuration for file events (ready/error)
///
/// This stream persists file ready/error events to ensure:
/// - READ_LATEST waiters receive notifications even if they temporarily disconnect
/// - File error events (from watchdog lock reclaim) are reliably delivered
/// - Agents can resume waiting after reconnection
///
/// Phase 5: Used by watchdog to broadcast file.error events on lock reclaim
pub fn file_events_stream_config() -> Config {
    Config {
        name: FILE_EVENTS_STREAM.to_string(),
        subjects: vec![
            "ergatai.file.ready.*".to_string(), // File ready (WRITE completed)
            "ergatai.file.error.*".to_string(), // File error (writer crashed)
        ],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(3600), // 1 hour (waiters should not wait too long)
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Create JetStream stream configuration for lock waiters queue
///
/// This stream manages the lock waiting queue to ensure:
/// - Agents waiting for locks are processed in FIFO order
/// - Lock release notifications are reliably delivered
/// - Fair scheduling when multiple agents compete for the same lock
/// - Persistent queue survives process restarts
///
/// Subjects:
/// - ergatai.lock.request.{file_hash}: Lock acquisition requests
/// - ergatai.lock.release.{file_hash}: Lock release notifications
/// - ergatai.lock.granted.{session_id}: Lock grant notifications (point-to-point)
pub fn lock_waiters_stream_config() -> Config {
    Config {
        name: LOCK_WAITERS_STREAM.to_string(),
        subjects: vec![
            "ergatai.lock.request.*".to_string(), // Lock requests (WorkQueue)
            "ergatai.lock.release.*".to_string(), // Lock releases (Pub/Sub)
        ],
        retention: RetentionPolicy::WorkQueue,
        max_age: Duration::from_secs(7200), // 2 hours (longer timeout for waiting)
        storage: StorageType::File,
        num_replicas: 1,
        ..Default::default()
    }
}

/// List of all file access JetStream stream configurations
///
/// Use this to initialize all required streams at startup.
pub fn all_file_access_stream_configs() -> Vec<Config> {
    vec![
        file_access_request_stream_config(),
        file_access_grant_stream_config(),
        file_access_escalate_stream_config(),
        file_events_stream_config(),  // Phase 5: file ready/error events
        lock_waiters_stream_config(), // Lock waiting queue
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_configs() {
        let configs = all_file_access_stream_configs();
        assert_eq!(configs.len(), 5); // Updated: now includes LOCK_WAITERS

        let request_config = file_access_request_stream_config();
        assert_eq!(request_config.name, "FILE_ACCESS_REQUESTS");
        assert_eq!(request_config.subjects, vec!["ergatai.file.access.request"]);

        let grant_config = file_access_grant_stream_config();
        assert_eq!(grant_config.name, "FILE_ACCESS_GRANTS");
        assert_eq!(grant_config.subjects, vec!["ergatai.file.access.grant.*"]);

        let escalate_config = file_access_escalate_stream_config();
        assert_eq!(escalate_config.name, "FILE_ACCESS_ESCALATIONS");
        assert_eq!(
            escalate_config.subjects,
            vec!["ergatai.file.access.escalate.*"]
        );

        // Phase 5: file events stream
        let events_config = file_events_stream_config();
        assert_eq!(events_config.name, "FILE_EVENTS");
        assert_eq!(events_config.subjects.len(), 2);
        assert!(events_config
            .subjects
            .contains(&"ergatai.file.ready.*".to_string()));
        assert!(events_config
            .subjects
            .contains(&"ergatai.file.error.*".to_string()));

        // Lock waiters stream
        let waiters_config = lock_waiters_stream_config();
        assert_eq!(waiters_config.name, "LOCK_WAITERS");
        assert_eq!(waiters_config.subjects.len(), 2);
        assert!(waiters_config
            .subjects
            .contains(&"ergatai.lock.request.*".to_string()));
        assert!(waiters_config
            .subjects
            .contains(&"ergatai.lock.release.*".to_string()));
    }

    #[test]
    fn test_file_events_stream_config() {
        let config = file_events_stream_config();
        assert_eq!(config.name, "FILE_EVENTS");
        assert_eq!(config.subjects.len(), 2);
        assert_eq!(config.subjects[0], "ergatai.file.ready.*");
        assert_eq!(config.subjects[1], "ergatai.file.error.*");
        assert_eq!(config.retention, RetentionPolicy::WorkQueue);
        assert_eq!(config.max_age, Duration::from_secs(3600));
        assert_eq!(config.storage, StorageType::File);
    }

    #[test]
    fn test_lock_waiters_stream_config() {
        let config = lock_waiters_stream_config();
        assert_eq!(config.name, "LOCK_WAITERS");
        assert_eq!(config.subjects.len(), 2);
        assert_eq!(config.subjects[0], "ergatai.lock.request.*");
        assert_eq!(config.subjects[1], "ergatai.lock.release.*");
        assert_eq!(config.retention, RetentionPolicy::WorkQueue);
        assert_eq!(config.max_age, Duration::from_secs(7200)); // 2 hours
        assert_eq!(config.storage, StorageType::File);
    }
}
