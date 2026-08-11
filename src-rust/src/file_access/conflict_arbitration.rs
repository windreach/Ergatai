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

    #[test]
    fn test_invalid_priority_defaults_to_medium() {
        // Invalid priority strings default to medium (2)
        // "unknown_invalid" (medium=2) vs "low" (1) → current holds (2 > 1)
        let conflict = create_test_conflict(Some("unknown_invalid"), Some("low"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_both_invalid_priorities_treated_as_equal() {
        // Both invalid → both default to medium (2) → same priority → first-come-first-served
        let conflict = create_test_conflict(Some("garbage"), Some("nonsense"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_case_insensitive_priority() {
        // "HIGH" and "high" should be treated the same (both = 3)
        let conflict = create_test_conflict(Some("HIGH"), Some("high"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );

        // Mixed case
        let conflict2 = create_test_conflict(Some("High"), Some("Low"));
        assert_eq!(
            arbitrate_conflict(&conflict2),
            ArbitrationDecision::KeepWithCurrentHolder
        );
    }

    #[test]
    fn test_medium_vs_low_priority() {
        let conflict = create_test_conflict(Some("medium"), Some("low"));
        assert_eq!(
            arbitrate_conflict(&conflict),
            ArbitrationDecision::KeepWithCurrentHolder
        );

        let conflict2 = create_test_conflict(Some("low"), Some("medium"));
        assert_eq!(
            arbitrate_conflict(&conflict2),
            ArbitrationDecision::GrantToNewRequester
        );
    }

    #[test]
    fn test_priority_to_number_known_values() {
        assert_eq!(priority_to_number(&Some("high".to_string())), Some(3));
        assert_eq!(priority_to_number(&Some("medium".to_string())), Some(2));
        assert_eq!(priority_to_number(&Some("low".to_string())), Some(1));
        assert_eq!(priority_to_number(&None), None);
    }

    #[test]
    fn test_priority_to_number_unknown_defaults_to_medium() {
        assert_eq!(priority_to_number(&Some("unknown".to_string())), Some(2));
        assert_eq!(priority_to_number(&Some("".to_string())), Some(2));
        assert_eq!(priority_to_number(&Some("critical".to_string())), Some(2));
    }

    #[test]
    fn test_generate_conflict_report_contains_key_fields() {
        let conflict = create_test_conflict(Some("high"), Some("low"));
        let decision = arbitrate_conflict(&conflict);
        let report = generate_conflict_report(&conflict, decision);

        assert!(report.contains("File Access Conflict Report"));
        assert!(report.contains("test.rs"));
        assert!(report.contains("agent-a"));
        assert!(report.contains("agent-b"));
        assert!(report.contains("session-a"));
        assert!(report.contains("session-b"));
        assert!(report.contains("2026-08-10T12:00:00Z"));
        assert!(report.contains("KeepWithCurrentHolder"));
    }

    #[test]
    fn test_generate_conflict_report_with_none_priority() {
        let conflict = create_test_conflict(None, None);
        let decision = arbitrate_conflict(&conflict);
        let report = generate_conflict_report(&conflict, decision);

        assert!(report.contains("None"));
        assert!(report.contains("Current task"));
        assert!(report.contains("New task"));
    }

    #[test]
    fn test_conflict_info_serialization_roundtrip() {
        let conflict = create_test_conflict(Some("high"), Some("low"));
        let json = serde_json::to_string(&conflict).unwrap();
        let deserialized: ConflictInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_path, conflict.file_path);
        assert_eq!(
            deserialized.current_holder.agent_id,
            conflict.current_holder.agent_id
        );
        assert_eq!(
            deserialized.new_requester.priority,
            conflict.new_requester.priority
        );
    }

    #[test]
    fn test_arbitration_decision_serialization_roundtrip() {
        let decisions = vec![
            ArbitrationDecision::GrantToNewRequester,
            ArbitrationDecision::KeepWithCurrentHolder,
            ArbitrationDecision::RejectBoth,
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision).unwrap();
            let deserialized: ArbitrationDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, decision);
        }
    }
}
