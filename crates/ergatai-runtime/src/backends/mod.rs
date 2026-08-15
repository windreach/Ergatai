//! Backend implementations for the Agent Runtime.
//!
//! Each module provides a concrete `AgentRuntimeBackend` implementation:
//! - `local_pty`: tmux-based local PTY management (original default)
//! - `rmux`: rmux SDK-based terminal multiplexer (preferred)
//! - `direct_process`: direct process spawning without terminal multiplexer

pub mod direct_process;
pub mod local_pty;
pub mod rmux;
