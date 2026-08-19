//! MCP (Model Context Protocol) module
//!
//! Implements the MCP server for agent communication using rmcp SDK.
//! Supports MCP protocol 2025-06-18 with Streamable HTTP transport.

pub mod batch_aggregator;
pub mod conversation;
pub mod message_delivery;
pub mod server;

// Re-export AgentRegistry for backward compatibility
pub use batch_aggregator::get_batch_aggregator;
pub use conversation::start_conversation_reaper;
pub use ergatai_core::agent_registry::AgentRegistry;
pub use message_delivery::start_message_delivery_consumer;
pub use server::create_mcp_service;
pub use server::start_peer_reaper;
