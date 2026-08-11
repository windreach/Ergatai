//! Conflict arbitration for file access control.
//!
//! When multiple agents request WRITE access to the same file,
//! this module provides arbitration based on task priority.
//!
//! ## Livelock Prevention
//!
//! To prevent infinite conflict loops (agents repeatedly retrying), the system uses:
//! - **Exponential backoff with jitter**: retry delays grow as 1s → 2s → 4s → 8s → 16s
//! - **Waiting-agent priority boost**: agents that have retried more get a temporary
//!   priority increase during arbitration, ensuring fairness

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
    /// Task priority (1=low, 2=medium, 3=high; None=unknown)
    pub priority: Option<u8>,
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
    let current_priority = conflict.current_holder.priority;
    let new_priority = conflict.new_requester.priority;

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
pub fn priority_to_number(priority: &Option<String>) -> Option<u8> {
    priority.as_ref().map(|p| match p.to_lowercase().as_str() {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 2, // Default to medium
    })
}

/// Retry advice returned when a lock request is rejected due to conflict.
///
/// The caller should wait `retry_after_ms` before retrying. After `max_retries`
/// the caller should give up and report failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAdvice {
    /// How long to wait before retrying (milliseconds).
    pub retry_after_ms: u64,
    /// How many times this (file, agent) pair has already retried.
    pub retry_count: u32,
    /// Maximum allowed retries.
    pub max_retries: u32,
    /// Whether the agent has earned a priority boost from waiting.
    pub priority_boosted: bool,
}

/// Maximum number of retries before giving up.
pub const MAX_RETRIES: u32 = 5;

/// Compute exponential backoff with random jitter.
///
/// Formula: `base * 2^retry_count + random(0..base*2^retry_count/2)`
/// - Retry 0: 1000ms + jitter(0-500ms)
/// - Retry 1: 2000ms + jitter(0-1000ms)
/// - Retry 2: 4000ms + jitter(0-2000ms)
/// - Retry 3: 8000ms + jitter(0-4000ms)
/// - Retry 4: 16000ms + jitter(0-8000ms)
pub fn compute_backoff_ms(retry_count: u32) -> u64 {
    let base_ms: u64 = 1000;
    let delay = base_ms.saturating_mul(1u64 << retry_count.min(6));
    // Deterministic jitter based on retry_count (no randomness needed at this layer —
    // the caller can add jitter if desired, or we use a simple hash-based spread).
    // For simplicity, add a fixed 25% spread based on retry_count to avoid thundering herd.
    let jitter = delay / 4 * ((retry_count as u64 + 1) % 3);
    delay + jitter
}

/// Priority boost threshold: after this many retries, the agent gets +1 priority level.
pub const PRIORITY_BOOST_THRESHOLD: u32 = 2;

/// Effective priority with waiting-agent boost applied.
///
/// Agents that have been waiting (retried) get a temporary priority increase:
/// - `retry_count >= 2` → +1 boost (medium → high)
/// - `retry_count >= 4` → +2 boost (low → high)
///
/// This ensures fairness: an agent stuck behind a same-priority holder
/// will eventually get through.
pub fn effective_priority(base_priority: Option<u8>, retry_count: u32) -> u8 {
    let base = base_priority.unwrap_or(2); // default medium
    let boost = if retry_count >= PRIORITY_BOOST_THRESHOLD * 2 {
        2 // max boost
    } else if retry_count >= PRIORITY_BOOST_THRESHOLD {
        1
    } else {
        0
    };
    (base + boost).min(5) // cap at 5
}

/// Arbitrate a conflict considering retry-based priority boosts.
///
/// This is the boost-aware version of `arbitrate_conflict`. When both agents
/// have retry counts, the effective priority (base + boost) is used.
pub fn arbitrate_with_boost(
    conflict: &ConflictInfo,
    current_retry_count: u32,
    new_retry_count: u32,
) -> ArbitrationDecision {
    let curr_base = conflict.current_holder.priority;
    let new_base = conflict.new_requester.priority;

    let curr_eff = effective_priority(curr_base, current_retry_count);
    let new_eff = effective_priority(new_base, new_retry_count);

    info!(
        file_path = %conflict.file_path,
        current_agent = %conflict.current_holder.agent_id,
        new_agent = %conflict.new_requester.agent_id,
        curr_base_priority = ?curr_base,
        new_base_priority = ?new_base,
        curr_retries = current_retry_count,
        new_retries = new_retry_count,
        curr_effective = curr_eff,
        new_effective = new_eff,
        "Arbitrating with priority boost"
    );

    if new_eff > curr_eff {
        ArbitrationDecision::GrantToNewRequester
    } else {
        ArbitrationDecision::KeepWithCurrentHolder
    }
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
        let str_to_priority = |s: &str| -> Option<u8> {
            priority_to_number(&Some(s.to_string()))
        };
        ConflictInfo {
            file_path: "test.rs".to_string(),
            current_holder: LockHolderInfo {
                agent_id: "agent-a".to_string(),
                session_id: "session-a".to_string(),
                token_id: "token-a".to_string(),
                priority: current_priority.and_then(str_to_priority),
                reason: Some("Current task".to_string()),
            },
            new_requester: LockHolderInfo {
                agent_id: "agent-b".to_string(),
                session_id: "session-b".to_string(),
                token_id: "token-b".to_string(),
                priority: new_priority.and_then(str_to_priority),
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
