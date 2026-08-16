//! MCP integration bridge for the Agent Runtime.
//!
//! Provides a bridge between the runtime's message delivery and the MCP
//! notification system. When backend message injection fails (e.g., DirectProcessBackend
//! doesn't support it), the runtime falls back to sending MCP custom notifications
//! through this module.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, info};

use ergatai_error::{ErgataiError, ErgataiResult};

/// Bridge between AgentRuntime and MCP notification delivery.
///
/// Holds references to connected MCP peers so the runtime can push
/// `ergatai/message` custom notifications as a fallback when backend
/// injection is unavailable.
pub struct McpIntegration {
    /// Registry of MCP peer handlers, keyed by MCP agent ID.
    ///
    /// Uses type-erased closures so that `ergatai-runtime` doesn't need
    /// to depend on `rmcp` directly. The actual peer type is provided
    /// by `ergatai-api` at initialization time.
    peers: Arc<RwLock<HashMap<String, PeerEntry>>>,
}

/// Type-erased send function for MCP notifications.
/// Wrapped in Arc so it can be cloned out of the peer map and called without
/// holding the read lock (prevents self-deadlock if the closure triggers
/// register_peer/unregister_peer on a reconnect path).
type SendNotificationFn = Arc<
    Box<
        dyn Fn(
                &str,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = ErgataiResult<()>> + Send>>
            + Send
            + Sync,
    >,
>;

/// An entry in the MCP peer registry.
struct PeerEntry {
    /// Send a notification to this peer.
    send_fn: SendNotificationFn,
}

impl McpIntegration {
    /// Create a new empty MCP integration.
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a peer for an MCP agent.
    ///
    /// The `send_fn` closure sends a notification message to the peer.
    /// This type-erasure allows the runtime to avoid depending on rmcp directly.
    pub async fn register_peer<F, Fut>(&self, mcp_agent_id: String, send_fn: F)
    where
        F: Fn(&str) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ErgataiResult<()>> + Send + 'static,
    {
        let send_fn_erased: SendNotificationFn = Arc::new(Box::new(move |msg: &str| {
            let fut = send_fn(msg);
            Box::pin(fut)
                as std::pin::Pin<Box<dyn std::future::Future<Output = ErgataiResult<()>> + Send>>
        }));

        self.peers.write().await.insert(
            mcp_agent_id.clone(),
            PeerEntry {
                send_fn: send_fn_erased,
            },
        );

        debug!(
            mcp_agent_id = mcp_agent_id,
            "MCP peer registered in runtime"
        );
    }

    /// Unregister a peer when an MCP agent disconnects.
    pub async fn unregister_peer(&self, mcp_agent_id: &str) {
        self.peers.write().await.remove(mcp_agent_id);
        debug!(
            mcp_agent_id = mcp_agent_id,
            "MCP peer unregistered from runtime"
        );
    }

    /// Send a notification to an MCP agent.
    pub async fn send_notification(&self, mcp_agent_id: &str, message: &str) -> ErgataiResult<()> {
        // Clone the send_fn out of the map, then drop the read guard BEFORE awaiting.
        // This prevents self-deadlock if the closure triggers register_peer/unregister_peer.
        let send_fn = {
            let peers = self.peers.read().await;
            let entry = peers.get(mcp_agent_id).ok_or_else(|| {
                ErgataiError::internal(format!(
                    "Agent {} has no active MCP connection",
                    mcp_agent_id
                ))
            })?;
            entry.send_fn.clone()
        };

        (send_fn)(message).await?;

        info!(mcp_agent_id = mcp_agent_id, "Notification sent via MCP");
        Ok(())
    }

    /// List all registered MCP agent IDs.
    pub async fn list_peers(&self) -> Vec<String> {
        self.peers.read().await.keys().cloned().collect()
    }
}

impl Default for McpIntegration {
    fn default() -> Self {
        Self::new()
    }
}
