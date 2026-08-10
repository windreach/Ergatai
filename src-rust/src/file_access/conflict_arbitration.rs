//! Conflict arbitration for file access control.
//!
//! When multiple agents request WRITE access to the same file,
//! this module provides arbitration based on task priority.

use serde::{Deserialize, Serialize};
use tracing::info;

/// Conflict information for arbitration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// File path in conflict
    pub file_path: String,
    /// Current lock holder
    pub current_holder: LockHolderInfo,
    /// New requester
    pub new_requester: LockHolderInfo,
    /// Conflict timestamp
    pub timestamp: String,
}

/// Information about a lock holder or requester
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockHolderInfo {
    /// Agent ID
    pub agent_id: String,
    /// Session ID
    pub session_id: String,
    /// Token ID
    pub token_id: String,
    /// Task priority (if available)
    pub priority: Option<String>,
    /// Request reason
    pub reason: Option<String>,
}

/// Arbitration decision
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArbitrationDecision {
    /// Grant lock to new requester (preempt current holder)
    GrantToNewRequester,
    /// Keep lock with current holder (new requester waits)
    KeepWithCurrentHolder,
    /// Reject both (neither gets the lock)
    RejectBoth,
}

/// Arbitrate a conflict based on priority and other factors
///
/// # Arguments
/// * `conflict` - The conflict information
///
/// # Returns
/// The arbitration decision
pub fn arbitrate_conflict(conflict: &ConflictInfo) -> ArbitrationDecision {
    info!(
        file_path = %conflict.file_path,
        current_agent = %conflict.current_holder.agent_id,
        new_agent = %conflict.new_requester.agent_id,
        "Arbitrating WRITE lock conflict"
    );

    // Priority comparison (if both have priorities)
    let current_priority = priority_to_number(&conflict.current_holder.priority);
    let new_priority = priority_to_number(&conflict.new_requester.priority);

    match (current_priority, new_priority) {
        (Some(curr), Some(new)) => {
            // Both have priorities: higher priority wins
            if new > curr {
                info!(
                    new_priority = new,
                    current_priority = curr,
                    "New requester has higher priority, granting lock"
                );
                ArbitrationDecision::GrantToNewRequester
            } else if new < curr {
                info!(
                    new_priority = new,
                    current_priority = curr,
                    "Current holder has higher priority, keeping lock"
                );
                ArbitrationDecision::KeepWithCurrentHolder
            } else {
                // Same priority: first-come-first-served (keep with current)
                info!(
                    priority = curr,
                    "Same priority, keeping lock with current holder (first-come-first-served)"
                );
                ArbitrationDecision::KeepWithCurrentHolder
            }
        }
        (Some(_), None) => {
            // Current has priority, new doesn't: keep with current
            info!("Current holder has priority, new requester doesn't, keeping lock");
            ArbitrationDecision::KeepWithCurrentHolder
        }
        (None, Some(_)) => {
            // New has priority, current doesn't: grant to new
            info!("New requester has priority, current holder doesn't, granting lock");
            ArbitrationDecision::GrantToNewRequester
        }
        (None, None) => {
            // Neither has priority: first-come-first-served (keep with current)
            info!("Neither has priority, keeping lock with current holder (first-come-first-served)");
            ArbitrationDecision::KeepWithCurrentHolder
        }
    }
}

/// Convert priority string to number for comparison
///
/// Priority levels: "high" = 3, "medium" = 2, "low" = 1
fn priority_to_number(priority: &Option<String>) -> Option<u8> {
    priority.as_ref().map(|p| match p.to_lowercase().as_str() {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 2, // Default to medium
    })
}

/// Generate a conflict report for logging or escalation
pub fn generate_conflict_report(conflict: &ConflictInfo, decision: ArbitrationDecision) -> String {
    format!(
        "File Access Conflict Report\n\
         ==========================\n\
         File: {}\n\
         Timestamp: {}\n\n\
         Current Holder:\n\
         - Agent: {}\n\
         - Session: {}\n\
         - Priority: {:?}\n\
         - Reason: {:?}\n\n\
         New Requester:\n\
         - Agent: {}\n\
         - Session: {}\n\
         - Priority: {:?}\n\
         - Reason: {:?}\n\n\
         Decision: {:?}\n",
        conflict.file_path,
        conflict.timestamp,
        conflict.current_holder.agent_id,
        conflict.current_holder.session_id,
        conflict.current_holder.priority,
        conflict.current_holder.reason,
        conflict.new_requester.agent_id,
        conflict.new_requester.session_id,
        conflict.new_requester.priority,
        conflict.new_requester.reason,
        decision
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_conflict(
        current_priority: Option<&str>,
        new_priority: Option<&str>,
    ) -> ConflictInfo {
        ConflictInfo {
            file_path: "test.rs".to_string(),
            current_holder: LockHolderInfo {
                agent_id: "agent-a".to_string(),
                session_id: "session-a".to_string(),
                token_id: "token-a".to_string(),
                priority: current_priority.map(|s| s.to_string()),
                reason: Some("Current task".to_string()),
            },
            new_requester: LockHolderInfo {
                agent_id: "agent-b".to_string(),
                session_id: "session-b".to_string(),
                token_id: "token-b".to_string(),
                priority: new_priority.map(|s| s.to_string()),
                reason: Some("New task".to_string()),
            },
            timestamp: "2026-08-10T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_high_priority_wins() {
        let conflict = create_test_conflict(Some("low"), Some("high"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::GrantToNewRequester
        );
    }

    #[test]
    fn test_current_high_priority_keeps() {
        let conflict = create_test_conflict(Some("high"), Some("low"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_same_priority_first_come_first_served() {
        let conflict = create_test_conflict(Some("medium"), Some("medium"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_no_priority_first_come_first_served() {
        let conflict = create_test_conflict(None, None);
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_new_has_priority_current_doesnt() {
        let conflict = create_test_conflict(None, Some("high"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::GrantToNewRequester
        );
    }

    #[test]
    fn test_current_has_priority_new_doesnt() {
        let conflict = create_test_conflict(Some("high"), None);
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }
}
