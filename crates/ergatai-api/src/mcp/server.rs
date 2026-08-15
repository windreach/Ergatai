//! MCP Server implementation using rmcp (Rust MCP SDK)
//!
//! Implements MCP protocol 2025-06-18 with Streamable HTTP transport.
//! Agents connect via POST/GET /mcp and can call tools like list_agents,
//! send_message, submit_orchestration, etc.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, CustomNotification, InitializeRequestParams,
        InitializeResult, ServerCapabilities, ServerInfo, ServerNotification,
    },
    service::{Peer, RequestContext},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use ergatai_core::agent_registry::AgentRegistry;
use ergatai_runtime::get_agent_runtime;
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared registry of MCP peer handles for pushing notifications to agents.
/// Key: agent_id (e.g., "opencode@abcd1234")
/// Value: Peer handle for sending notifications to that agent's MCP session.
pub type PeerRegistry = Arc<RwLock<HashMap<String, Peer<RoleServer>>>>;

/// Create a new empty PeerRegistry.
pub fn new_peer_registry() -> PeerRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// MCP Server state - shared across all sessions via Arc
#[derive(Clone)]
pub struct ErgataiMcpServer {
    tool_router: ToolRouter<Self>,
    registry: Arc<AgentRegistry>,
    /// Shared peer registry for pushing notifications to agents
    peer_registry: PeerRegistry,
    /// Per-session agent ID (set during initialize, used in send_message)
    session_agent_id: Arc<RwLock<Option<String>>>,
    /// Tracks consecutive send_message failures per target agent.
    /// After `MAX_SEND_FAILURES` consecutive failures, the target is auto-unregistered.
    send_failures: Arc<RwLock<HashMap<String, u32>>>,
}

/// Maximum consecutive send failures before auto-unregistering a target agent.
const MAX_SEND_FAILURES: u32 = 3;

impl std::fmt::Debug for ErgataiMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErgataiMcpServer").finish_non_exhaustive()
    }
}

impl ErgataiMcpServer {
    /// Create a new server instance (called per-session by the factory)
    pub fn new(
        registry: Arc<AgentRegistry>,
        peer_registry: PeerRegistry,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
            peer_registry,
            session_agent_id: Arc::new(RwLock::new(None)),
            send_failures: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

/// When the per-session `ErgataiMcpServer` is dropped (session ends — client
/// disconnect, idle timeout, or server shutdown), automatically unregister the
/// agent from the shared registry and remove its peer handle. Without this,
/// dead agents accumulate as zombies because rmcp's `ServerHandler` has no
/// `on_close` callback.
impl Drop for ErgataiMcpServer {
    fn drop(&mut self) {
        // `Drop` is synchronous — use `try_read` (non-blocking) to grab the
        // agent ID, then spawn the async cleanup on the tokio runtime.
        // The session worker task is still on the runtime when it drops us,
        // so `tokio::spawn` is safe here.
        let agent_id = match self.session_agent_id.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                warn!(
                    "ErgataiMcpServer::drop: session_agent_id lock contended, \
                     skipping unregister (stale-agent reaper will clean up)"
                );
                None
            }
        };

        if let Some(agent_id) = agent_id {
            let registry = self.registry.clone();
            let peer_registry = self.peer_registry.clone();
            info!("MCP session ending, unregistering agent: {}", agent_id);
            tokio::spawn(async move {
                do_unregister_agent(&registry, &peer_registry, &agent_id, "MCP session closed").await;
            });
        }
    }
}

/// Unregister an agent from the registry and remove its peer handle.
/// Centralized helper used by Drop, peer reaper, and send_message failure handler.
async fn do_unregister_agent(
    registry: &AgentRegistry,
    peer_registry: &PeerRegistry,
    agent_id: &str,
    reason: &str,
) {
    registry.unregister_agent(agent_id).await;
    peer_registry.write().await.remove(agent_id);
    info!("Agent {} unregistered ({})", agent_id, reason);
}

// ── Tool parameter types ──

#[derive(Debug, Deserialize, JsonSchema)]
struct ListAgentsParams {
    /// Whether to include agent capabilities
    #[serde(default)]
    include_capabilities: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendMessageParams {
    /// ID of the target agent
    target_agent_id: String,
    /// Message content
    message: String,
    /// Type of message (request, response, broadcast)
    #[serde(default)]
    message_type: Option<String>,
}


#[derive(Debug, Deserialize, JsonSchema)]
struct SubmitOrchestrationParams {
    /// Markdown-formatted DAG definition
    dag_definition: String,
    /// Optional context variables
    #[serde(default)]
    context: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CheckDagStatusParams {
    /// DAG ID to check
    dag_id: String,
}

// ── Tool implementations ──

#[tool_router]
impl ErgataiMcpServer {
    /// List all connected agents and their status
    #[tool(description = "List all connected agents and their status")]
    async fn list_agents(
        &self,
        params: Parameters<ListAgentsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let include_capabilities = params.0.include_capabilities.unwrap_or(false);
        let agents = self.registry.list_agents().await;

        let agents_json: Vec<serde_json::Value> = agents
            .iter()
            .map(|agent| {
                let mut agent_json = serde_json::json!({
                    "agent_id": agent.agent_id,
                    "status": agent.status,
                    "connected_at": agent.connected_at,
                    "last_heartbeat": agent.last_heartbeat,
                });

                if include_capabilities {
                    if let Some(caps) = &agent.capabilities {
                        agent_json["capabilities"] = serde_json::json!(caps);
                    }
                }

                agent_json
            })
            .collect();

        let result = serde_json::json!({
            "agents": agents_json,
            "total": agents.len()
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Send a message to another agent.
    ///
    /// Delivery order (reliable):
    /// 1. **NATS JetStream** (preferred) — message is persisted to `AGENT_MESSAGES` stream,
    ///    then delivered by the background `MessageDeliveryConsumer` via tmux injection
    ///    or MCP notification. Provides durability, retry on failure, and delivery confirmation.
    /// 2. **Direct tmux injection** (fallback) — when NATS is unavailable, falls back to
    ///    direct tmux injection, then MCP notification. No persistence guarantee.
    #[tool(description = "Send a message to another agent (NATS JetStream for reliability, tmux/MCP fallback)")]
    async fn send_message(
        &self,
        params: Parameters<SendMessageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let target_agent_id = &params.0.target_agent_id;
        let message = &params.0.message;
        let message_type = params.0.message_type.as_deref().unwrap_or("request");

        info!(
            "Sending message to agent {}: {} (type: {})",
            target_agent_id, message, message_type
        );

        // Find the matching agent - support both exact ID and name prefix
        // Check both MCP registry and AgentRuntime
        let agents = self.registry.list_agents().await;
        let runtime = get_agent_runtime();
        let runtime_agents = runtime.list_agents().await;

        let matching_agent = agents
            .iter()
            .find(|a| {
                // Exact match
                a.agent_id == *target_agent_id
                // Or prefix match (e.g., "simple-agent" matches "simple-agent@ead00fad")
                || a.agent_id.starts_with(&format!("{}@", target_agent_id))
            })
            .map(|a| a.agent_id.clone())
            .or_else(|| {
                // Check runtime agents (by task_id or mcp_agent_id)
                runtime_agents
                    .iter()
                    .find(|a| {
                        a.agent_id == *target_agent_id
                        || a.task_id.as_deref() == Some(target_agent_id)
                        || a.mcp_agent_id.as_deref() == Some(target_agent_id)
                    })
                    .map(|a| a.agent_id.clone())
            });

        let resolved_agent_id = match matching_agent {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Agent {} not found. Agent must connect via MCP or be running in tmux.",
                    target_agent_id
                ))]));
            }
        };

        // Get the sender agent ID
        let from_agent = self.session_agent_id.read().await
            .clone()
            .unwrap_or_else(|| "unknown-mcp-client".to_string());

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // ── Primary path: publish to NATS JetStream (reliable) ──
        if let Some(conn) = ergatai_nats::get_nats_connection().await {
            let bus = ergatai_nats::EventBus::new(conn);
            let payload = ergatai_nats::AgentMessagePayload {
                from_agent: from_agent.clone(),
                to_agent: resolved_agent_id.clone(),
                content: message.to_string(),
                thread_id: None,
                timestamp,
                metadata: std::collections::HashMap::new(),
            };

            match bus.publish_agent_message_reliable(&payload).await {
                Ok(ack) => {
                    self.send_failures.write().await.remove(&resolved_agent_id);

                    let response_json = serde_json::json!({
                        "status": "queued",
                        "target_agent": resolved_agent_id,
                        "delivery_method": "nats_jetstream",
                        "stream": ack.stream,
                        "sequence": ack.sequence,
                        "note": "Message persisted to NATS JetStream. Background consumer will deliver via tmux injection (preferred) or MCP notification (fallback)."
                    });

                    return Ok(CallToolResult::success(vec![ContentBlock::text(
                        serde_json::to_string_pretty(&response_json).unwrap_or_default(),
                    )]));
                }
                Err(e) => {
                    warn!(
                        "NATS JetStream publish failed (falling back to direct delivery): {}",
                        e
                    );
                    // Fall through to direct delivery
                }
            }
        }

        // ── Fallback: direct tmux injection (no persistence) ──
        if let Ok(result) = self.try_tmux_injection(&resolved_agent_id, &from_agent, message).await {
            self.send_failures.write().await.remove(&resolved_agent_id);
            return Ok(result);
        }

        // ── Last resort: MCP custom notification ──
        self.send_mcp_notification(
            &resolved_agent_id,
            &from_agent,
            message,
            message_type,
            timestamp,
        )
        .await
    }

    /// Submit a DAG workflow for multi-agent collaboration
    #[tool(description = "Submit a DAG workflow for multi-agent collaboration")]
    async fn submit_orchestration(
        &self,
        params: Parameters<SubmitOrchestrationParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let dag_definition = &params.0.dag_definition;
        let context_value = &params.0.context;

        info!("Submitting DAG orchestration ({} bytes)", dag_definition.len());

        // Check if a DAG is already running
        if let Some(existing) = ergatai_core::cross_agent::get_dag_scheduler() {
            if !existing.is_complete().await {
                return Err(ErrorData::internal_error(
                    "A DAG is already running. Wait for it to complete or check its status.",
                    None,
                ));
            }
        }

        // Parse markdown → TaskGraph
        let graph = ergatai_core::orchestration::parse_dag_markdown(dag_definition)
            .map_err(|e| {
                ErrorData::invalid_params(
                    format!("Failed to parse DAG definition: {}", e),
                    None,
                )
            })?;

        // Build DagContext from optional context parameter
        let mut dag_context = ergatai_core::orchestration::DagContext::empty();
        if let Some(ctx_val) = context_value {
            if let Some(vars) = ctx_val.as_object() {
                for (k, v) in vars {
                    dag_context.set_global(
                        k.clone(),
                        v.as_str().unwrap_or_default().to_string(),
                    );
                }
            }
        }

        // Create DagScheduler
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let scheduler = ergatai_core::cross_agent::DagScheduler::with_context(
            project_root,
            graph,
            dag_context,
        );

        // Register globally + start NATS event listener
        ergatai_core::cross_agent::set_dag_scheduler(scheduler.clone());
        scheduler.clone().start_event_listener();

        // Submit the graph (dispatches ready nodes)
        let submitted = scheduler.submit_graph().await.map_err(|e| {
            ErrorData::internal_error(format!("Failed to submit DAG: {}", e), None)
        })?;

        let progress = scheduler.progress().await;
        let status = scheduler.status_prompt().await;

        let result = serde_json::json!({
            "status": "submitted",
            "submitted_nodes": submitted.len(),
            "progress": progress,
            "graph_status": status,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Check the status of a DAG execution
    #[tool(description = "Check the status of a DAG execution")]
    async fn check_dag_status(
        &self,
        params: Parameters<CheckDagStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let _dag_id = &params.0.dag_id;

        info!("Checking DAG status");

        match ergatai_core::cross_agent::get_dag_scheduler() {
            None => {
                let result = serde_json::json!({
                    "status": "no_dag",
                    "message": "No DAG scheduler is active",
                });
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                )]))
            }
            Some(scheduler) => {
                let progress = scheduler.progress().await;
                let is_complete = scheduler.is_complete().await;
                let status_text = scheduler.status_prompt().await;
                let snapshot = scheduler.graph_snapshot().await.ok();

                let status = if is_complete { "completed" } else { "running" };

                let result = serde_json::json!({
                    "status": status,
                    "progress": progress,
                    "is_complete": is_complete,
                    "graph_status": status_text,
                    "graph_snapshot": snapshot,
                });
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                )]))
            }
        }
    }

    // ── Private helpers for send_message ──

    /// Inject message via AgentRuntime (preferred method when agent is tracked).
    async fn try_tmux_injection(
        &self,
        resolved_agent_id: &str,
        from_agent: &str,
        message: &str,
    ) -> Result<CallToolResult, ErrorData> {
        // Format the message with sender info
        let formatted_message = format!(
            "Message from {}: {}",
            from_agent, message
        );

        info!(
            "Attempting AgentRuntime injection to agent {}: {}",
            resolved_agent_id, formatted_message
        );

        // Try to inject via AgentRuntime (uses backend injection or MCP fallback)
        let runtime = get_agent_runtime();
        match runtime.inject_message(resolved_agent_id, &formatted_message).await {
            Ok(()) => {
                info!("Message injected to {} via AgentRuntime", resolved_agent_id);
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "sent",
                        "target_agent": resolved_agent_id,
                        "delivery_method": "runtime_injection",
                        "note": "Message injected via AgentRuntime (backend injection or MCP fallback)."
                    }))
                    .unwrap_or_default(),
                )]))
            }
            Err(e) => {
                warn!("AgentRuntime injection to {} failed: {}", resolved_agent_id, e);
                Err(ErrorData::internal_error(
                    format!("AgentRuntime injection failed: {}", e),
                    None,
                ))
            }
        }
    }

    /// Send MCP custom notification (fallback when tmux injection is unavailable).
    async fn send_mcp_notification(
        &self,
        resolved_agent_id: &str,
        from_agent: &str,
        message: &str,
        message_type: &str,
        timestamp: u64,
    ) -> Result<CallToolResult, ErrorData> {
        let peer = {
            let peers = self.peer_registry.read().await;
            peers.get(resolved_agent_id).cloned()
        };

        let peer = match peer {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Agent {} has no active MCP connection. \
                     Cannot deliver message.",
                    resolved_agent_id
                ))]));
            }
        };

        let payload = serde_json::json!({
            "from_agent": from_agent,
            "to_agent": resolved_agent_id,
            "content": message,
            "message_type": message_type,
            "timestamp": timestamp,
        });

        let notification = CustomNotification::new("ergatai/message", Some(payload));

        match peer
            .send_notification(ServerNotification::CustomNotification(notification))
            .await
        {
            Ok(_) => {
                self.send_failures
                    .write()
                    .await
                    .remove(resolved_agent_id);

                let message_id = uuid::Uuid::new_v4().to_string();
                let response_json = serde_json::json!({
                    "message_id": message_id,
                    "status": "delivered",
                    "target_agent_id": resolved_agent_id,
                    "message_type": message_type,
                    "delivery_method": "mcp_notification",
                    "note": "Delivered via MCP notification. \
                             Target agent must handle 'ergatai/message' custom notifications to see this message."
                });

                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&response_json).unwrap_or_default(),
                )]))
            }
            Err(e) => {
                error!(
                    "Failed to send MCP notification to {}: {}",
                    resolved_agent_id, e
                );

                let mut failures = self.send_failures.write().await;
                let count = failures.entry(resolved_agent_id.to_string()).or_insert(0);
                *count += 1;
                let current = *count;

                if current >= MAX_SEND_FAILURES {
                    warn!(
                        "Agent {} reached {}/{} consecutive send failures, auto-unregistering",
                        resolved_agent_id, current, MAX_SEND_FAILURES
                    );
                    let registry = self.registry.clone();
                    let peer_registry = self.peer_registry.clone();
                    let agent_id = resolved_agent_id.to_string();
                    failures.remove(resolved_agent_id);
                    tokio::spawn(async move {
                        do_unregister_agent(
                            &registry,
                            &peer_registry,
                            &agent_id,
                            &format!("{} consecutive send failures", MAX_SEND_FAILURES),
                        )
                        .await;
                    });
                    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "Agent {} has been auto-unregistered after {} consecutive send failures.",
                        resolved_agent_id, MAX_SEND_FAILURES
                    ))]))
                } else {
                    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "Failed to deliver message to {} ({}/{} consecutive failures)",
                        resolved_agent_id, current, MAX_SEND_FAILURES
                    ))]))
                }
            }
        }
    }
}

// ── ServerHandler implementation ──

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ErgataiMcpServer {
    /// Handle initialize - auto-register the agent and save peer handle
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let agent_id = request.client_info.name.clone();
        let agent_version = request.client_info.version.clone();

        // Generate a connection ID - use as unique agent key to support
        // multiple instances of the same client (e.g. 3 OpenCode instances)
        let connection_id = uuid::Uuid::new_v4().to_string();
        // Take first 8 chars of UUID (safe: UUIDs are always 36 chars: 8-4-4-4-12)
        let id_prefix = connection_id.get(..8).unwrap_or(&connection_id);
        let unique_agent_id = format!("{}@{}", agent_id, id_prefix);

        info!(
            "Agent connecting: {} (version: {}, protocol: {}) → {}",
            agent_id, agent_version, request.protocol_version, unique_agent_id
        );

        // Store the agent ID for this session (used in send_message)
        *self.session_agent_id.write().await = Some(unique_agent_id.clone());

        // Register agent in registry
        if let Err(e) = self.registry
            .register_agent(unique_agent_id.clone(), connection_id.clone(), None)
            .await
        {
            return Err(ErrorData::invalid_params(format!("Failed to register agent: {}", e), None::<serde_json::Value>));
        }

        // Note: AgentRuntime backend (LocalPtyBackend) handles pane discovery
        // via scan_and_register_panes() during initialization. MCP agents that
        // need tmux mapping should be launched via AgentRuntime.

        // Save the peer handle for pushing notifications to this agent
        self.peer_registry.write().await.insert(
            unique_agent_id.clone(),
            context.peer.clone(),
        );

        info!(
            "Agent registered: {} (connection: {}, peer handle saved)",
            unique_agent_id, connection_id
        );

        // Build the initialize result
        let mut server_info = self.get_info();
        // Negotiate: use client's version if we know it, otherwise our latest
        let client_version = &request.protocol_version;
        let known = rmcp::model::ProtocolVersion::KNOWN_VERSIONS
            .iter()
            .any(|v| v.as_str() == client_version.as_str());
        server_info.protocol_version = if known {
            client_version.clone()
        } else {
            rmcp::model::ProtocolVersion::default()
        };

        // Store peer info in context
        context.peer.set_peer_info(request);

        Ok(server_info)
    }

    /// Return server info with tools capability
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                rmcp::model::Implementation::new("ergatai", env!("CARGO_PKG_VERSION"))
            )
    }
}

// ── Public API for creating the Streamable HTTP service ──

use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};

/// Create the MCP Streamable HTTP service for mounting in axum.
///
/// Returns a `StreamableHttpService` that handles POST/GET/DELETE /mcp
/// with proper MCP 2025-06-18 protocol support.
///
/// # Arguments
/// * `registry` - Agent registry for tracking connected agents
/// * `peer_registry` - Shared registry of MCP peer handles for pushing notifications
/// * `cancellation_token` - Token for graceful shutdown
/// * `sse_keep_alive_secs` - SSE keep-alive interval in seconds (default 15)
pub fn create_mcp_service(
    registry: Arc<AgentRegistry>,
    peer_registry: PeerRegistry,
    cancellation_token: CancellationToken,
    sse_keep_alive_secs: u64,
) -> StreamableHttpService<ErgataiMcpServer, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(std::time::Duration::from_secs(sse_keep_alive_secs)))
        .with_sse_retry(Some(std::time::Duration::from_secs(3)))
        .with_json_response(true)
        .with_cancellation_token(cancellation_token)
        .with_allowed_hosts([
            "localhost", "127.0.0.1", "::1", "0.0.0.0",
        ]);

    // Session keep_alive: auto-close sessions after this duration of inactivity.
    // This catches dead clients (kill, network drop) within 2 minutes.
    // Default is 300s (5 min). Agents that call tools periodically stay alive.
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(std::time::Duration::from_secs(120));

    StreamableHttpService::new(
        move || Ok(ErgataiMcpServer::new(registry.clone(), peer_registry.clone())),
        std::sync::Arc::new(session_manager),
        config,
    )
}

/// Start a background task that periodically checks all peer connections
/// and removes agents whose MCP transport has been closed (e.g. abrupt disconnect).
///
/// This complements the `Drop`-based cleanup which only fires on graceful session close.
/// When a client is killed (SIGKILL, network drop), the SSE session may linger until
/// the transport detects the broken connection. The reaper proactively cleans these up.
pub fn start_peer_reaper(
    registry: Arc<AgentRegistry>,
    peer_registry: PeerRegistry,
    cancellation_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    info!("Peer reaper shutting down");
                    break;
                }
                _ = interval.tick() => {
                    let stale_peers: Vec<String> = {
                        let peers = peer_registry.read().await;
                        peers.iter()
                            .filter(|(_, peer)| peer.is_transport_closed())
                            .map(|(id, _)| id.clone())
                            .collect()
                    };

                    for agent_id in stale_peers {
                        warn!("Peer reaper: detected dead transport for {}, cleaning up", agent_id);
                        do_unregister_agent(
                            &registry, &peer_registry, &agent_id, "dead transport (reaper)",
                        ).await;
                    }
                }
            }
        }
    });
}
