//! Binary locator and manager for Ergatai external dependencies
//!
//! This crate provides a unified mechanism for locating and managing external
//! binary dependencies like NATS server and rmux daemon.
//!
//! # Architecture
//!
//! The `BinaryLocator` implements a multi-layer search strategy:
//! 1. Environment variable override (e.g., `ERGATAI_NATS_BINARY`)
//! 2. Bundled resources directory (downloaded by build.rs at compile time)
//! 3. Sibling directory (next to the executable)
//! 4. System PATH (development fallback)
//!
//! # rmux Integration
//!
//! rmux-daemon is bundled with ergatai and automatically managed:
//! - build.rs downloads pre-built rmux-daemon binaries during compilation
//! - `ensure_rmux_daemon()` locates the binary and auto-starts the daemon
//! - rmux-sdk automatically connects to the running daemon
//!
//! # Example
//!
//! ```no_run
//! use ergatai_binary::{find_nats_binary, ensure_rmux_daemon};
//!
//! // Find NATS server binary
//! let nats_path = find_nats_binary().expect("NATS binary not found");
//!
//! // Locate rmux-daemon and auto-start if needed
//! let rmux_path = ensure_rmux_daemon(true).expect("rmux-daemon not found");
//! ```

mod finder;
mod nats;
mod rmux;
mod tmux;

pub use finder::BinaryLocator;
pub use nats::find_nats_binary;
pub use rmux::{
    configure_rmux_daemon, ensure_rmux_daemon, get_daemon_path, is_daemon_running,
    is_rmux_available,
};
pub use tmux::{find_tmux_binary, is_tmux_available};
