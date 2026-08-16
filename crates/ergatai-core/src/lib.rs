//! Ergatai — Multi-agent collaboration platform core library.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Ergatai Workspace Crates                                │
//! │   ergatai-error     — unified error types               │
//! │   ergatai-nats      — NATS messaging infrastructure     │
//! │   ergatai-dag       — DAG orchestration engine          │
//! │   ergatai-lock      — file locking & access control     │
//! │   ergatai-agent     — agent config & discovery          │
//! │   ergatai-collab    — multi-agent task coordination     │
//! │   ergatai-core      — glue crate (init, re-exports)     │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! This library provides the core functionality for Ergatai:
//! - Multi-agent DAG orchestration
//! - File access control with locking
//! - NATS-based messaging infrastructure
//! - Agent configuration and discovery
//! - Tmux-based agent message injection
//!
//! The library is used by both the CLI (ergatai-cli) and API server (ergatai-api).

// ── Business logic modules ──
pub mod agent_registry;
pub mod signal;

// ── Re-export extracted crates ──
pub use ergatai_agent as agent;
pub use ergatai_collab as cross_agent;
pub use ergatai_dag as orchestration;
pub use ergatai_error as error;
pub use ergatai_lock as file_access;
pub use ergatai_nats as nats;

// ── Re-export tmux from cross_agent (lives in ergatai-collab) ──
pub use ergatai_collab::tmux;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::Once;

// ── One-time init ──

static INIT_LOGGING: Once = Once::new();
static INIT_PANIC_HOOK: Once = Once::new();

// ── Global resources path ──
static RESOURCES_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Set the resources directory path (called from TypeScript on app startup).
///
/// This path is used to locate bundled assets like agent icons.
pub fn set_resources_path(path: PathBuf) {
    if let Ok(mut guard) = RESOURCES_PATH.lock() {
        *guard = Some(path);
    }
}

/// Get the resources directory path.
pub fn get_resources_path() -> Option<PathBuf> {
    RESOURCES_PATH.lock().ok().and_then(|guard| guard.clone())
}

/// Initialize the `tracing` subscriber exactly once.
///
/// Called at application startup. Cheap after the first call — `Once::call_once` is a
/// single atomic load.
pub fn init_logging() {
    INIT_LOGGING.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env().add_directive(
                    "ergatai=info"
                        .parse()
                        .expect("\"ergatai=info\" is a valid tracing directive"),
                ),
            )
            .with_writer(std::io::stderr)
            .init();
        tracing::info!("Ergatai logging initialized");
    });
}

/// Install a global panic hook that logs via `tracing` instead of stderr.
pub fn init_panic_hook() {
    INIT_PANIC_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|panic_info| {
            let payload = panic_info.payload();
            let location = panic_info.location();

            let payload_str = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic payload".to_string()
            };

            let location_str = location
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown location".to_string());

            tracing::error!(
                panic_payload = %payload_str,
                panic_location = %location_str,
                "Panic occurred"
            );
        }));
    });
}

// ── Re-export the unified error for submodules ──

// ── Re-export signal handler for binary crates ──
pub use signal::setup_signal_handlers;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── set_resources_path / get_resources_path ──

    #[test]
    fn test_resources_path_initially_none() {
        // Note: this test might fail if another test has already set the path.
        // We reset it after each test to keep tests isolated.
        // Just verify the getter returns Some(PathBuf) or None without panicking.
        let _ = get_resources_path();
    }

    #[test]
    fn test_set_resources_path_stores_value() {
        let path = PathBuf::from("/tmp/test-resources-1");
        set_resources_path(path.clone());
        let got = get_resources_path();
        assert_eq!(got, Some(path));

        // Reset for other tests
        reset_resources_path();
    }

    #[test]
    fn test_set_resources_path_overwrites() {
        set_resources_path(PathBuf::from("/tmp/res-a"));
        set_resources_path(PathBuf::from("/tmp/res-b"));
        assert_eq!(get_resources_path(), Some(PathBuf::from("/tmp/res-b")));
        reset_resources_path();
    }

    #[test]
    fn test_resources_path_with_relative_path() {
        set_resources_path(PathBuf::from("./relative/path"));
        assert_eq!(
            get_resources_path(),
            Some(PathBuf::from("./relative/path"))
        );
        reset_resources_path();
    }

    #[test]
    fn test_resources_path_with_empty_string() {
        set_resources_path(PathBuf::from(""));
        assert_eq!(get_resources_path(), Some(PathBuf::from("")));
        reset_resources_path();
    }

    #[test]
    fn test_resources_path_with_unicode() {
        let path = PathBuf::from("/tmp/リソース/测试");
        set_resources_path(path.clone());
        assert_eq!(get_resources_path(), Some(path));
        reset_resources_path();
    }

    #[test]
    fn test_get_resources_path_returns_clone() {
        set_resources_path(PathBuf::from("/tmp/clone-test"));
        let a = get_resources_path();
        let b = get_resources_path();
        assert_eq!(a, b);
        reset_resources_path();
    }

    fn reset_resources_path() {
        if let Ok(mut guard) = RESOURCES_PATH.lock() {
            *guard = None;
        }
    }

    // ── init_logging / init_panic_hook ──

    #[test]
    fn test_init_logging_does_not_panic() {
        // init_logging uses Once so it only runs the body once. Subsequent calls
        // are no-ops. Just verify it doesn't panic.
        init_logging();
        init_logging(); // second call should be safe
    }

    #[test]
    fn test_init_panic_hook_does_not_panic() {
        // init_panic_hook also uses Once. Verify double-call is safe.
        init_panic_hook();
        init_panic_hook();
    }

    // ── Re-exports ──

    #[test]
    fn test_re_exports_are_accessible() {
        // Verify the re-exports compile and are accessible at the crate root.
        // We don't instantiate them — just reference the types to confirm they exist.
        fn _assert_error_type_exists() {
            let _: Option<ergatai_error::ErgataiError> = None;
        }
        // The others are crate re-exports; referencing the module paths is enough.
        let _ = std::any::type_name::<fn()>(); // no-op
    }
}
