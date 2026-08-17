//! rmux daemon binary locator and auto-start configuration
//!
//! This module provides functionality to:
//! 1. Locate the rmux-daemon binary (bundled or system)
//! 2. Configure the RMUX_SDK_DAEMON_BINARY environment variable
//! 3. Auto-start the daemon if not running

use crate::finder::BinaryLocator;
use ergatai_error::ErgataiResult;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

static RMUX_LOCATOR: BinaryLocator = BinaryLocator {
    name: "rmux",
    env_override: Some("ERGATAI_RMUX_BINARY"),
    resource_subdir_pattern: Some("rmux-{platform}"),
};

/// One-shot initialization result.
///
/// `Ok(path)` on success, `Err(message)` on failure. Storing `String` (rather
/// than `ErgataiError`) avoids requiring `Clone` on the error type while still
/// preserving the diagnostic message across repeated callers.
///
/// # Concurrency / env-var safety
///
/// The init closure calls `std::env::set_var("RMUX_SDK_DAEMON_BINARY", ...)`.
/// On Rust 1.80+ `set_var` is documented as potentially racy with concurrent
/// `env::var` readers. To keep this safe, `ergatai-api`'s `main.rs` invokes
/// `ensure_rmux_daemon(true)` **before** the tokio runtime starts — so the
/// env write happens while only the main thread exists. Subsequent calls
/// (including from async contexts like `daemon_info`) hit the cached value
/// and do not re-enter the closure.
static RMUX_INIT: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Find rmux-daemon binary, configure environment, and auto-start if needed.
///
/// This is the primary entry point for rmux integration. It:
/// 1. Locates the rmux-daemon binary (bundled → sibling → system PATH)
/// 2. Sets `RMUX_SDK_DAEMON_BINARY` env var for rmux-sdk to discover
/// 3. Optionally starts the daemon if not already running
///
/// # Returns
///
/// The path to the rmux-daemon binary on success.
///
/// # Blocking / async-safety
///
/// The first call performs a binary lookup and an env-var write. It must
/// happen **before** any threads are spawned (in practice, `ergatai-api`'s
/// `main.rs` calls this pre-runtime). The auto-start step (`auto_start = true`)
/// is non-blocking: it spawns the daemon and returns immediately; a background
/// std thread performs the diagnostic "did it exit right away?" check.
///
/// Subsequent calls return the cached path without any blocking.
///
/// # Example
///
/// ```no_run
/// use ergatai_binary::ensure_rmux_daemon;
///
/// let daemon_path = ensure_rmux_daemon(true).expect("rmux-daemon not available");
/// // Now rmux-sdk will automatically use this daemon
/// ```
pub fn ensure_rmux_daemon(auto_start: bool) -> ErgataiResult<PathBuf> {
    let path = configure_rmux_daemon()?;

    if auto_start {
        start_daemon_if_needed(&path)?;
    }

    Ok(path)
}

/// Find rmux-daemon binary and set RMUX_SDK_DAEMON_BINARY environment variable.
///
/// This function is idempotent - calling it multiple times returns the same path.
/// The underlying lookup + env write runs at most once (cached in `RMUX_INIT`).
///
/// # Safety note (env-var write)
///
/// The first call writes `RMUX_SDK_DAEMON_BINARY` via `std::env::set_var`.
/// The caller must ensure this happens before any threads are spawned that
/// might read environment variables concurrently. `ergatai-api`'s `main.rs`
/// enforces this by calling `ensure_rmux_daemon(true)` pre-runtime.
pub fn configure_rmux_daemon() -> ErgataiResult<PathBuf> {
    RMUX_INIT
        .get_or_init(|| {
            match RMUX_LOCATOR.find() {
                Ok(path) => {
                    // See the safety note on RMUX_INIT for why this is acceptable.
                    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &path);
                    Ok(path)
                }
                Err(e) => {
                    let msg = e.to_string();
                    tracing::error!("Failed to locate rmux-daemon: {}", msg);
                    Err(msg)
                }
            }
        })
        .clone()
        .map_err(|msg| {
            ergatai_error::ErgataiError::internal(format!(
                "rmux-daemon not found (initialization failed: {msg}). \
                 Set ERGATAI_RMUX_BINARY or ensure bundled binary exists",
            ))
        })
}

/// Check if rmux-daemon is available (without setting env var or starting).
pub fn is_rmux_available() -> bool {
    RMUX_LOCATOR.find().is_ok()
}

/// Check if rmux-daemon is currently running.
pub fn is_daemon_running() -> bool {
    check_daemon_running(None)
}

/// Check daemon status using a specific rmux CLI binary, falling back to PATH.
fn check_daemon_running(rmux_cli: Option<&Path>) -> bool {
    let cmd = match rmux_cli {
        Some(path) => {
            if path.exists() {
                path
            } else {
                // Caller specified an explicit path that doesn't exist —
                // daemon is not running at that location.
                return false;
            }
        }
        None => Path::new("rmux"),
    };
    Command::new(cmd)
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Start the rmux-daemon if not already running.
///
/// The daemon runs in the background and manages terminal sessions.
/// This function returns immediately after spawning (or confirming it's running).
///
/// # Non-blocking
///
/// Spawning the daemon is synchronous, but the diagnostic wait that checks
/// whether the daemon exited immediately is performed on a dedicated std
/// thread. This keeps `start_daemon_if_needed` safe to call from async
/// contexts (e.g., `RmuxBackend::restart_daemon`) without stalling the
/// executor.
fn start_daemon_if_needed(rmux_path: &Path) -> ErgataiResult<()> {
    // Check if daemon is already running (using the specific bundled CLI)
    if check_daemon_running(Some(rmux_path)) {
        tracing::debug!("rmux-daemon is already running");
        return Ok(());
    }

    tracing::info!(
        path = %rmux_path.display(),
        "Starting rmux daemon"
    );

    // Start daemon using `rmux daemon --background`
    // This ensures rmux sets up its libexec helpers correctly
    let result = Command::new(rmux_path)
        .arg("daemon")
        .arg("--background")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match result {
        Ok(child) => {
            // Hand the child off to a dedicated std thread for the diagnostic
            // wait. This keeps the current function non-blocking for callers
            // (including async contexts like `restart_daemon`).
            std::thread::spawn(move || {
                let mut child = child;
                std::thread::sleep(std::time::Duration::from_millis(500));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::warn!(
                            status = %status,
                            "rmux daemon exited immediately (another instance may already be running)"
                        );
                    }
                    Ok(None) => {
                        tracing::info!("rmux daemon started successfully");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to check daemon status");
                    }
                }
                // `child` is dropped here; on Unix the zombie is reaped by init.
            });
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %rmux_path.display(),
                "Failed to start rmux daemon. rmux-sdk may auto-start it on first use."
            );
            // Don't fail - rmux-sdk has its own auto-start logic
        }
    }

    Ok(())
}

/// Get the path to the configured rmux-daemon binary.
///
/// Returns `None` if `configure_rmux_daemon()` hasn't been called yet,
/// or if initialization failed.
pub fn get_daemon_path() -> Option<PathBuf> {
    RMUX_INIT.get().and_then(|r| r.as_ref().ok()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rmux_locator_static_config() {
        // Verify the static locator is configured correctly
        assert_eq!(RMUX_LOCATOR.name, "rmux");
        assert_eq!(RMUX_LOCATOR.env_override, Some("ERGATAI_RMUX_BINARY"));
        assert_eq!(
            RMUX_LOCATOR.resource_subdir_pattern,
            Some("rmux-{platform}")
        );
    }

    #[test]
    fn test_check_daemon_running_with_nonexistent_path() {
        // A nonexistent path should return false
        let result = check_daemon_running(Some(Path::new("/nonexistent/rmux/binary")));
        assert!(!result);
    }

    #[test]
    fn test_check_daemon_running_with_none_fallback() {
        // When None is passed, it falls back to "rmux" on PATH
        // This should not panic even if rmux isn't installed
        let _ = check_daemon_running(None);
    }

    #[test]
    fn test_is_rmux_available_does_not_panic() {
        // Should not panic regardless of whether rmux is installed
        let _ = is_rmux_available();
    }

    #[test]
    fn test_is_daemon_running_does_not_panic() {
        // Should not panic regardless of whether daemon is running
        let _ = is_daemon_running();
    }

    #[test]
    fn test_get_daemon_path_before_init() {
        // If configure_rmux_daemon hasn't been called, get_daemon_path returns None.
        // However, since RMUX_INIT is a OnceLock, other tests may have triggered init.
        // This test just verifies the function doesn't panic.
        let _ = get_daemon_path();
    }

    #[test]
    fn test_start_daemon_if_needed_nonexistent_path() {
        // Should not panic or return error even with nonexistent path
        // (rmux-sdk has its own auto-start logic, so this is lenient)
        let result = start_daemon_if_needed(Path::new("/nonexistent/rmux"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_configure_rmux_daemon_is_idempotent() {
        // Calling configure multiple times should return same result
        // (OnceLock guarantees this)
        let r1 = configure_rmux_daemon();
        let r2 = configure_rmux_daemon();
        // Both should succeed or both should fail
        assert_eq!(r1.is_ok(), r2.is_ok());
        if let (Ok(p1), Ok(p2)) = (&r1, &r2) {
            assert_eq!(p1, p2);
        }
    }

    #[test]
    fn test_env_override_variable_name() {
        // The env var should match what's documented
        assert_eq!(RMUX_LOCATOR.env_override.unwrap(), "ERGATAI_RMUX_BINARY");
    }
}
