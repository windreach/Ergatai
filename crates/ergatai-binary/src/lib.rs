//! Binary locator and manager for Ergatai external dependencies
//!
//! This crate provides a unified mechanism for locating and managing external
//! binary dependencies like NATS server and rmux daemon.
//!
//! # Architecture
//!
//! The `BinaryLocator` implements a 3-layer search strategy:
//! 1. Environment variable override (e.g., `ERGATAI_NATS_BINARY`)
//! 2. Bundled resources directory (for packaged releases)
//! 3. System PATH (development fallback)
//!
//! # Example
//!
//! ```no_run
//! use ergatai_binary::{find_nats_binary, configure_rmux_daemon};
//!
//! // Find NATS server binary
//! let nats_path = find_nats_binary().expect("NATS binary not found");
//!
//! // Configure rmux daemon (sets RMUX_SDK_DAEMON_BINARY env var)
//! let rmux_path = configure_rmux_daemon().expect("rmux daemon not found");
//! ```

mod finder;
mod nats;
mod rmux;

pub use finder::BinaryLocator;
pub use nats::find_nats_binary;
pub use rmux::{configure_rmux_daemon, is_rmux_available};
