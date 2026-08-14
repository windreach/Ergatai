//! Permission approval policy for ACP permission requests.
//!
//! Controls whether permission requests from agents are automatically approved
//! or require explicit user consent. In production, this should be set to
//! `manual` or `policy` to prevent unauthorized operations.

use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag: when true, all permission requests are auto-approved.
/// Default is false (secure by default).
static AUTO_APPROVE: AtomicBool = AtomicBool::new(false);

/// Set whether permission requests should be auto-approved.
///
/// # Arguments
/// * `enabled` - If true, all permission requests are automatically approved.
///   If false, permission requests will be denied (until a proper approval UI is implemented).
pub fn set_auto_approve(enabled: bool) {
    AUTO_APPROVE.store(enabled, Ordering::SeqCst);
    if enabled {
        tracing::warn!(
            "⚠️  Auto-approve ENABLED for permission requests. \
             All agent permission requests will be automatically granted."
        );
    } else {
        tracing::info!(
            "Permission auto-approve DISABLED. \
             Permission requests will be denied until approval UI is implemented."
        );
    }
}

/// Check if auto-approve is currently enabled.
pub fn is_auto_approve() -> bool {
    AUTO_APPROVE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_secure() {
        // Reset to default for test isolation
        set_auto_approve(false);
        assert!(!is_auto_approve());
    }

    #[test]
    fn test_set_and_check() {
        set_auto_approve(true);
        assert!(is_auto_approve());
        set_auto_approve(false);
        assert!(!is_auto_approve());
    }
}
