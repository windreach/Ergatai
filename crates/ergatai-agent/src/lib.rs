//! Ergatai Agent — placeholder crate.
//!
//! # Migration Note
//!
//! This crate previously contained agent hosting logic (config, discovery,
//! installation, runtime metadata). In the middleware architecture, agents
//! manage their own lifecycle and connect to Ergatai via MCP/ACP.
//!
//! Agent tracking is now handled by `ergatai-api::mcp::AgentRegistry`.
//!
//! This crate is kept as a placeholder for potential future agent-related
//! utilities that don't involve hosting.
