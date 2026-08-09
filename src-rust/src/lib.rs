//! Ergatai — Rust core for the 21st Agents desktop app.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ NAPI layer (`napi/`)                                    │
//! │   #[napi] functions, type conversion, logging guard     │
//! │   - acp.rs   - mcp.rs                     │
//! │   - skills.rs - tasks.rs - dag.rs                                │
//! └───────────────────────┬─────────────────────────────────┘
//!                         │  (thin wrappers)
//! ┌───────────────────────▼─────────────────────────────────┐
//! │ Business logic                                          │
//! │   acp/          — ACP session management (SDK-based)    │
//! │   agent/        — agent config              │
//! │   cross_agent/  — multi-agent task coordination         │
//! │   mcp.rs        — MCP server config scanning            │
//! │   skills.rs     — skill file discovery                  │
//! └───────────────────────┬─────────────────────────────────┘
//!                         │
//! ┌───────────────────────▼─────────────────────────────────┐
//! │ error.rs — unified error enum (ErgataiError)            │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! The NAPI layer is purely glue — it handles:
//! - one-time logging / panic-hook init (via `napi::guard()`)
//! - `ErgataiError → napi::Error` conversion
//! - exposing `#[napi]` functions to the JS runtime
//!
//! All business logic lives in the submodules and is NAPI-agnostic.

// ── Business logic modules ──
pub mod error;
pub mod acp;
pub mod agent;
pub mod cross_agent;
pub mod orchestration;
pub mod nats;

// Internal modules (not exported to NAPI directly)
mod skills;
mod mcp;

// ── NAPI binding layer ──
pub mod napi;

use std::sync::Once;
use std::path::PathBuf;
use std::sync::Mutex;

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
/// Called (transitively) at the start of every NAPI entry point via
/// `napi::guard()`. Cheap after the first call — `Once::call_once` is a
/// single atomic load.
pub(crate) fn init_logging() {
    INIT_LOGGING.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("ergatai=info".parse().expect("\"ergatai=info\" is a valid tracing directive")),
            )
            .with_writer(std::io::stderr)
            .init();
        tracing::info!("Ergatai logging initialized");
    });
}

/// Install a global panic hook that logs via `tracing` instead of stderr.
pub(crate) fn init_panic_hook() {
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
