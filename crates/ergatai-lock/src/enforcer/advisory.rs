//! Advisory-only backend for unsupported platforms or degraded mode.
//!
//! This backend never intercepts file access. It is used as a fallback when
//! no mandatory enforcement mechanism is available (non-Linux, missing privileges,
//! container without CAP_SYS_ADMIN, etc.).

use async_trait::async_trait;

use super::backend::{EnforcementResult, EnforcerBackend, FileAccessEvent, PlatformHandle};

/// Advisory-only backend that never intercepts file access.
///
/// Used as a fallback when no platform-specific enforcement mechanism is
/// available. The enforcer facade detects `is_mandatory() == false` and
/// skips spawning the event loop entirely.
pub struct AdvisoryBackend;

#[async_trait]
impl EnforcerBackend for AdvisoryBackend {
    fn name(&self) -> &'static str {
        "advisory"
    }

    fn is_mandatory(&self) -> bool {
        false
    }

    async fn next_event(&self) -> Option<FileAccessEvent> {
        // Advisory backend never produces events.
        // The facade should check is_mandatory() and skip the event loop.
        // This return ensures the loop terminates if somehow called.
        None
    }

    async fn respond(
        &self,
        _handle: PlatformHandle,
        _result: EnforcementResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // No kernel response needed in advisory mode.
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Nothing to clean up.
        Ok(())
    }
}
