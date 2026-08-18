//! Platform backend trait and shared types for cross-platform file access enforcement.
//!
//! This module defines the abstraction layer that allows the enforcer to use
//! platform-specific mechanisms (fanotify on Linux, Endpoint Security on macOS,
//! Minifilter on Windows) through a unified interface.

use async_trait::async_trait;
use std::path::PathBuf;

/// Events emitted by platform backends to the facade.
///
/// Each platform intercepts file access differently, but all produce
/// the same logical event: "process X tried to access file Y."
#[derive(Debug, Clone)]
pub struct FileAccessEvent {
    /// Absolute path to the file being accessed.
    pub absolute_path: PathBuf,
    /// PID of the process attempting the access.
    pub pid: u32,
    /// Opaque platform-specific handle that must be passed to `respond()`.
    /// For fanotify: the event fd. For ES: the message identifier.
    /// For minifilter: the callback data pointer.
    pub platform_handle: PlatformHandle,
}

/// Opaque handle for platform-specific kernel response.
///
/// Wraps whatever token the platform needs to allow/deny the specific
/// access request. The facade never inspects this — it passes it back
/// to the backend's `respond()` method.
#[derive(Debug, Clone)]
pub enum PlatformHandle {
    /// Linux fanotify: (group_fd, event_fd) pair.
    #[cfg(target_os = "linux")]
    Fanotify { group_fd: i32, event_fd: i32 },

    /// macOS Endpoint Security: message identifier.
    #[cfg(target_os = "macos")]
    EndpointSecurity { message_id: u64 },

    /// Windows Minifilter: opaque callback data pointer.
    #[cfg(target_os = "windows")]
    Minifilter { callback_data: u64 },

    /// Advisory-only: no kernel response needed.
    Advisory,
}

/// Result of an enforcement decision, returned to the backend for
/// kernel-level response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementResult {
    /// Allow the file access.
    Allow,
    /// Deny the file access.
    Deny,
}

/// Trait implemented by each platform's file-access interception mechanism.
///
/// # Lifecycle
///
/// 1. `start()` — initialize the platform mechanism, begin intercepting
/// 2. `next_event()` — async; yields the next file access event
/// 3. `respond()` — allow or deny a specific event at the kernel level
/// 4. `stop()` — tear down the mechanism, release all resources
///
/// # Fail-open contract
///
/// If `start()` fails, the backend returns `Ok(None)` from `next_event()`
/// immediately (stream terminates). The facade interprets this as
/// "disable enforcement" and transitions to advisory mode.
///
/// Implementations MUST ensure that `respond()` with `EnforcementResult::Allow`
/// is always reachable — even if the decision engine panics, the backend
/// must allow the access (fail-open). This is enforced via RAII guards
/// in each platform implementation.
#[async_trait]
pub trait EnforcerBackend: Send + Sync + 'static {
    /// Human-readable name for logging (e.g., "fanotify", "endpoint-security").
    fn name(&self) -> &'static str;

    /// Whether this backend provides mandatory enforcement (true) or is
    /// advisory-only (false). Used for health checks and status reporting.
    fn is_mandatory(&self) -> bool;

    /// Yield the next file access event. Returns `None` when the backend
    /// has shut down or cannot intercept (fail-open).
    ///
    /// This is the main event-polling method. The facade's event loop
    /// calls this in a loop.
    async fn next_event(&self) -> Option<FileAccessEvent>;

    /// Respond to a file access event at the kernel level.
    ///
    /// MUST be called for every event yielded by `next_event()`. The
    /// `platform_handle` from the event is passed back here.
    ///
    /// # Fail-open
    ///
    /// If this method errors, the implementation MUST still release the
    /// kernel-blocked process (allow it). Logging the error is sufficient.
    async fn respond(
        &self,
        handle: PlatformHandle,
        result: EnforcementResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Stop the backend and release all platform resources.
    ///
    /// After `stop()` returns, `next_event()` must return `None`.
    /// Idempotent — calling `stop()` multiple times is safe.
    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}
