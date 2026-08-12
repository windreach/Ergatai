//! Lock waiter data structures for NATS-based waiting queue
//!
//! Defines the message types used for lock waiting and notification
//! through the NATS JetStream LOCK_WAITERS stream.

use serde::{Deserialize, Serialize};
use crate::file_access::FileMode;

/// Lock wait request published to ergatai.lock.request.{file_hash}
///
/// When an agent tries to acquire a lock that is already held,
/// it publishes this request to the LOCK_WAITERS stream and waits
/// for a LockGrantedNotification on its reply subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockWaitRequest {
    /// Unique request identifier (UUID)
    pub request_id: String,
    /// File token ID requesting the lock
    pub token_id: String,
    /// Agent identifier
    pub agent_id: String,
    /// ACP session identifier
    pub session_id: String,
    /// File path being requested
    pub file_path: String,
    /// Lock mode (READ/WRITE/ADMIN)
    pub mode: FileMode,
    /// Timestamp when request was created (RFC3339)
    pub timestamp: String,
    /// Optional priority for conflict arbitration
    pub priority: Option<String>,
    /// Subject to receive grant notification (ergatai.lock.granted.{session_id})
    pub reply_subject: String,
}

/// Lock release notification published to ergatai.lock.release.{file_hash}
///
/// When an agent releases a lock, this notification is published to
/// wake up any waiters for that file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockReleaseNotification {
    /// File path that was released
    pub file_path: String,
    /// Token ID that released the lock
    pub released_by_token_id: String,
    /// Timestamp when lock was released (RFC3339)
    pub released_at: String,
}

/// Lock grant notification sent to ergatai.lock.granted.{session_id}
///
/// When a waiter is next in queue and the lock is available,
/// this notification is sent to wake up the waiting agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockGrantedNotification {
    /// Request ID being granted (matches LockWaitRequest.request_id)
    pub request_id: String,
    /// File path being granted
    pub file_path: String,
    /// Timestamp when lock was granted (RFC3339)
    pub granted_at: String,
    /// Expiration time for the grant (agents should acquire within this window)
    pub expires_at: String,
}

impl LockWaitRequest {
    /// Create a new lock wait request
    pub fn new(
        token_id: String,
        agent_id: String,
        session_id: String,
        file_path: String,
        mode: FileMode,
        priority: Option<String>,
    ) -> Self {
        let request_id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let reply_subject = format!("ergatai.lock.granted.{}", session_id);

        Self {
            request_id,
            token_id,
            agent_id,
            session_id,
            file_path,
            mode,
            timestamp,
            priority,
            reply_subject,
        }
    }

    /// Get the NATS subject for this request
    pub fn subject(&self) -> String {
        let file_hash = md5_hash(&self.file_path);
        format!("ergatai.lock.request.{}", file_hash)
    }
}

impl LockReleaseNotification {
    /// Create a new lock release notification
    pub fn new(file_path: String, released_by_token_id: String) -> Self {
        let released_at = chrono::Utc::now().to_rfc3339();

        Self {
            file_path,
            released_by_token_id,
            released_at,
        }
    }

    /// Get the NATS subject for this notification
    pub fn subject(&self) -> String {
        let file_hash = md5_hash(&self.file_path);
        format!("ergatai.lock.release.{}", file_hash)
    }
}

impl LockGrantedNotification {
    /// Create a new lock grant notification
    pub fn new(request_id: String, file_path: String) -> Self {
        let granted_at = chrono::Utc::now().to_rfc3339();
        let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(30))
            .to_rfc3339();

        Self {
            request_id,
            file_path,
            granted_at,
            expires_at,
        }
    }

    /// Get the NATS subject for a specific session
    pub fn subject_for_session(session_id: &str) -> String {
        format!("ergatai.lock.granted.{}", session_id)
    }
}

/// Compute a simple hash of file path for NATS subject
/// Uses FNV-1a hash algorithm (fast, simple, no external dependencies)
fn md5_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_wait_request_creation() {
        let request = LockWaitRequest::new(
            "token-123".to_string(),
            "agent-a".to_string(),
            "session-a".to_string(),
            "src/main.rs".to_string(),
            FileMode::Write,
            Some("high".to_string()),
        );

        assert!(!request.request_id.is_empty());
        assert_eq!(request.token_id, "token-123");
        assert_eq!(request.agent_id, "agent-a");
        assert_eq!(request.session_id, "session-a");
        assert_eq!(request.file_path, "src/main.rs");
        assert_eq!(request.reply_subject, "ergatai.lock.granted.session-a");
        assert!(request.timestamp.contains("T")); // RFC3339 format
    }

    #[test]
    fn test_lock_wait_request_subject() {
        let request = LockWaitRequest::new(
            "token-123".to_string(),
            "agent-a".to_string(),
            "session-a".to_string(),
            "src/main.rs".to_string(),
            FileMode::Write,
            None,
        );

        let subject = request.subject();
        assert!(subject.starts_with("ergatai.lock.request."));
        assert_eq!(subject.len(), "ergatai.lock.request.".len() + 16); // FNV-1a hash is 16 chars (64-bit)
    }

    #[test]
    fn test_lock_release_notification() {
        let notification = LockReleaseNotification::new(
            "src/main.rs".to_string(),
            "token-123".to_string(),
        );

        assert_eq!(notification.file_path, "src/main.rs");
        assert_eq!(notification.released_by_token_id, "token-123");
        assert!(notification.released_at.contains("T"));

        let subject = notification.subject();
        assert!(subject.starts_with("ergatai.lock.release."));
    }

    #[test]
    fn test_lock_granted_notification() {
        let notification = LockGrantedNotification::new(
            "req-456".to_string(),
            "src/main.rs".to_string(),
        );

        assert_eq!(notification.request_id, "req-456");
        assert_eq!(notification.file_path, "src/main.rs");
        assert!(notification.granted_at.contains("T"));
        assert!(notification.expires_at.contains("T"));

        let subject = LockGrantedNotification::subject_for_session("session-a");
        assert_eq!(subject, "ergatai.lock.granted.session-a");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let request = LockWaitRequest::new(
            "token-123".to_string(),
            "agent-a".to_string(),
            "session-a".to_string(),
            "src/main.rs".to_string(),
            FileMode::Write,
            Some("high".to_string()),
        );

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: LockWaitRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.request_id, request.request_id);
        assert_eq!(deserialized.token_id, request.token_id);
        assert_eq!(deserialized.agent_id, request.agent_id);
    }
}
