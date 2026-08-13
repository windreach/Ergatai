//! Ergatai — Multi-agent collaboration platform core library.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Business logic                                          │
//! │   acp/          — ACP session management (SDK-based)    │
//! │   agent/        — agent config and discovery            │
//! │   cross_agent/  — multi-agent task coordination         │
//! │   file_access/  — file locking and access control       │
//! │   nats/         — NATS messaging infrastructure         │
//! │   orchestration/— DAG orchestration engine              │
//! │   mcp.rs        — MCP server config scanning            │
//! │   skills.rs     — skill file discovery                  │
//! └───────────────────────┬─────────────────────────────────┘
//!                         │
//! ┌───────────────────────▼─────────────────────────────────┐
//! │ ergatai-error — unified error enum (ErgataiError)       │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! This library provides the core functionality for Ergatai:
//! - ACP (Agent Client Protocol) session management
//! - Multi-agent DAG orchestration
//! - File access control with locking
//! - NATS-based messaging infrastructure
//! - Agent configuration and discovery
//!
//! The library is used by both the CLI (ergatai-cli) and API server (ergatai-api).

// ── Business logic modules ──
pub mod acp;
pub mod agent;
pub mod cross_agent;
pub mod file_access;
pub mod orchestration;
pub mod signal;

// Internal modules
mod mcp;
mod skills;

// ── Re-export extracted crates ──
pub use ergatai_error as error;
pub use ergatai_nats as nats;

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
