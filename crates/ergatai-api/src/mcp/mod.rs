//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent communication using rmcp SDK.
//! Supports MCP protocol 2025-06-18 with Streamable HTTP transport.

pub mod message_relay;
pub mod server;
pub mod message_forwarder;

// Re-export AgentRegistry from ergatai-acp for backward compatibility
pub use ergatai_acp::agent_registry::AgentRegistry;
pub use server::create_mcp_service;
pub use server::start_peer_reaper;
pub use message_forwarder::start_nats_acp_forwarder;
