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

#[cfg(test)]
mod tests {
    use super::*;
    use ergatai_error::ErgataiError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_mcp_integration_new() {
        let mcp = McpIntegration::new();
        // Just verify construction
        let _ = format!("{:p}", &mcp);
    }

    #[test]
    fn test_mcp_integration_default() {
        let mcp = McpIntegration::default();
        let _ = format!("{:p}", &mcp);
    }

    #[tokio::test]
    async fn test_register_and_list_peers() {
        let mcp = McpIntegration::new();
        mcp.register_peer("peer-1".to_string(), |_| async { Ok(()) })
            .await;
        mcp.register_peer("peer-2".to_string(), |_| async { Ok(()) })
            .await;
        let mut peers = mcp.list_peers().await;
        peers.sort();
        assert_eq!(peers, vec!["peer-1".to_string(), "peer-2".to_string()]);
    }

    #[tokio::test]
    async fn test_unregister_peer() {
        let mcp = McpIntegration::new();
        mcp.register_peer("peer-1".to_string(), |_| async { Ok(()) })
            .await;
        mcp.register_peer("peer-2".to_string(), |_| async { Ok(()) })
            .await;
        mcp.unregister_peer("peer-1").await;
        let peers = mcp.list_peers().await;
        assert_eq!(peers, vec!["peer-2".to_string()]);
    }

    #[tokio::test]
    async fn test_unregister_nonexistent_peer() {
        let mcp = McpIntegration::new();
        // Should not panic
        mcp.unregister_peer("nonexistent").await;
    }

    #[tokio::test]
    async fn test_send_notification_success() {
        let mcp = McpIntegration::new();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let received_clone = received.clone();
        mcp.register_peer("peer-1".to_string(), move |msg: &str| {
            let received = received_clone.clone();
            let msg = msg.to_string();
            async move {
                received.lock().await.push(msg);
                Ok(())
            }
        })
        .await;
        mcp.send_notification("peer-1", "hello").await.unwrap();
        let messages = received.lock().await.clone();
        assert_eq!(messages, vec!["hello".to_string()]);
    }

    #[tokio::test]
    async fn test_send_notification_unknown_peer() {
        let mcp = McpIntegration::new();
        let result = mcp.send_notification("unknown", "hello").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("no active MCP connection"));
    }

    #[tokio::test]
    async fn test_send_notification_callback_error() {
        let mcp = McpIntegration::new();
        mcp.register_peer("peer-1".to_string(), |_msg: &str| async {
            Err(ErgataiError::internal("send failed".to_string()))
        })
        .await;
        let result = mcp.send_notification("peer-1", "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_peer_overwrites() {
        let mcp = McpIntegration::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        mcp.register_peer("peer-1".to_string(), move |_: &str| {
            let c = c1.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;

        let counter2 = Arc::new(AtomicUsize::new(0));
        let c2 = counter2.clone();
        mcp.register_peer("peer-1".to_string(), move |_: &str| {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;

        mcp.send_notification("peer-1", "hello").await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_notifications() {
        let mcp = McpIntegration::new();
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let received_clone = received.clone();
        mcp.register_peer("peer-1".to_string(), move |msg: &str| {
            let received = received_clone.clone();
            let msg = msg.to_string();
            async move {
                received.lock().await.push(msg);
                Ok(())
            }
        })
        .await;
        for i in 0..5 {
            mcp.send_notification("peer-1", &format!("msg-{}", i))
                .await
                .unwrap();
        }
        let messages = received.lock().await.clone();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0], "msg-0");
        assert_eq!(messages[4], "msg-4");
    }
}
