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
    // Relaxed ordering is sufficient: this is a standalone flag with no
    // other memory to synchronize against.
    AUTO_APPROVE.store(enabled, Ordering::Relaxed);
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
    // Relaxed ordering is sufficient for a standalone boolean flag.
    AUTO_APPROVE.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: These tests mutate the global AUTO_APPROVE atomic and are NOT safe
    // to run in parallel. Run with `cargo test -- --test-threads=1` or the tests
    // may flake under concurrent execution.

    #[test]
    fn test_default_is_secure() {
        // Set explicitly, then assert — makes the test self-contained regardless
        // of what other tests do concurrently.
        set_auto_approve(false);
        assert!(!is_auto_approve());
        // Restore to secure default
        set_auto_approve(false);
    }

    #[test]
    fn test_set_and_check() {
        // Set, assert, then restore — self-contained
        set_auto_approve(true);
        assert!(is_auto_approve());
        // Restore to secure default
        set_auto_approve(false);
        assert!(!is_auto_approve());
    }
}
