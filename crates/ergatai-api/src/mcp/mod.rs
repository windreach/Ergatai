//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent communication.

pub mod types;
pub mod agent_registry;
pub mod tools;
pub mod server;

pub use server::McpServer;
pub use agent_registry::AgentRegistry;
pub use types::*;
