//! Three-stage node timeout escalation.
//!
//! A node's timeout budget is split into three tiers:
//!   - Warn      (50%): informational, logged at WARN level.
//!   - Escalate  (80%): logged at ERROR level so operators notice.
//!   - Fail     (100%): node is marked Failed with a `timeout_error`
//!     metadata entry, and `on_node_failed` is invoked.
//!
//! Only the Fail tier mutates node state; the first two are observability signals.

/// Tier of node-timeout escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutTier {
    /// 50% of node timeout elapsed — informational warning.
    Warn,
    /// 80% of node timeout elapsed — escalated, logged at WARN level.
    Escalate,
    /// 100% of node timeout elapsed — node failed.
    Fail,
}

impl TimeoutTier {
    /// Fraction of total timeout at which this tier fires.
    pub fn fraction(&self) -> f64 {
        match self {
            TimeoutTier::Warn => 0.5,
            TimeoutTier::Escalate => 0.8,
            TimeoutTier::Fail => 1.0,
        }
    }

    /// Compute the three absolute deadlines (warn, escalate, fail) from now.
    pub fn deadline_from_now(
        total_timeout_secs: u64,
    ) -> (std::time::Instant, std::time::Instant, std::time::Instant) {
        let now = std::time::Instant::now();
        let total = std::time::Duration::from_secs(total_timeout_secs);
        (
            now + total.mul_f64(Self::Warn.fraction()),
            now + total.mul_f64(Self::Escalate.fraction()),
            now + total,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_are_ordered() {
        assert!(TimeoutTier::Warn.fraction() < TimeoutTier::Escalate.fraction());
        assert!(TimeoutTier::Escalate.fraction() < TimeoutTier::Fail.fraction());
    }

    #[test]
    fn deadline_from_now_orders_warn_before_escalate_before_fail() {
        let (warn_at, escalate_at, fail_at) = TimeoutTier::deadline_from_now(10);
        assert!(warn_at < escalate_at);
        assert!(escalate_at < fail_at);
        // Fail deadline should be ~10s in the future.
        let remaining = fail_at.duration_since(std::time::Instant::now());
        assert!(remaining.as_secs() >= 9 && remaining.as_secs() <= 11);
    }
}
