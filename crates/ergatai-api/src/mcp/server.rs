//! MCP Server implementation using rmcp (Rust MCP SDK)
//!
//! Implements MCP protocol 2025-06-18 with Streamable HTTP transport.
//! Agents connect via POST/GET /mcp and can call tools like list_agents,
//! send_message, set_acp_endpoint, etc.

use std::sync::Arc;

use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, InitializeRequestParams, InitializeResult,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use ergatai_acp::agent_registry::AgentRegistry;
use ergatai_nats::{get_nats_connection, is_nats_initialized, AgentMessagePayload, EventBus};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::message_relay;

/// MCP Server state - shared across all sessions via Arc
#[derive(Clone)]
pub struct ErgataiMcpServer {
    tool_router: ToolRouter<Self>,
    registry: Arc<AgentRegistry>,
    /// Per-session agent ID (set during initialize, used in send_message)
    session_agent_id: Arc<RwLock<Option<String>>>,
    /// Ergatai's own address (for endpoint validation)
    ergatai_own_address: Arc<String>,
}

impl std::fmt::Debug for ErgataiMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErgataiMcpServer").finish_non_exhaustive()
    }
}

impl ErgataiMcpServer {
    /// Create a new server instance (called per-session by the factory)
    pub fn new(registry: Arc<AgentRegistry>, ergatai_own_address: Arc<String>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
            session_agent_id: Arc::new(RwLock::new(None)),
            ergatai_own_address,
        }
    }
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
struct SetAcpEndpointParams {
    /// ACP HTTP endpoint URL (e.g., "http://localhost:8080")
    endpoint: String,
    /// Agent ID (optional, defaults to the calling agent's ID)
    #[serde(default)]
    agent_id: Option<String>,
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

                if let Some(ref endpoint) = agent.acp_endpoint {
                    agent_json["acp_endpoint"] = serde_json::json!(endpoint);
                }

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
    /// Message is routed via NATS and delivered to the target agent's ACP endpoint.
    /// **Target agent MUST have an ACP endpoint registered to receive messages.**
    #[tool(description = "Send a message to another agent via NATS routing")]
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
        let agents = self.registry.list_agents().await;
        let matching_agent = agents
            .iter()
            .find(|a| {
                // Exact match
                a.agent_id == *target_agent_id
                // Or prefix match (e.g., "simple-agent" matches "simple-agent@ead00fad")
                || a.agent_id.starts_with(&format!("{}@", target_agent_id))
            })
            .map(|a| a.agent_id.clone());

        let resolved_agent_id = match matching_agent {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Agent {} not found. Agent must connect via MCP first.",
                    target_agent_id
                ))]));
            }
        };

        // Check if target agent has an ACP endpoint (REQUIRED)
        let acp_endpoint = self.registry.get_acp_endpoint(&resolved_agent_id).await;

        if acp_endpoint.is_none() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Agent {} has no ACP endpoint registered. \
                 Agents MUST register their ACP endpoint via set_acp_endpoint to receive messages.",
                resolved_agent_id
            ))]));
        }

        // Route message via NATS
        info!("Routing message to {} via NATS", resolved_agent_id);

        if !is_nats_initialized().await {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "NATS not initialized. Cannot route message.".to_string(),
            )]));
        }

        let conn = match get_nats_connection().await {
            Some(c) => c,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "NATS connection not available.".to_string(),
                )]));
            }
        };

        let bus = EventBus::new(conn);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Get the actual sender agent ID from the session
        let from_agent = self.session_agent_id.read().await
            .clone()
            .unwrap_or_else(|| "unknown-mcp-client".to_string());

        let payload = AgentMessagePayload {
            from_agent,
            to_agent: resolved_agent_id.clone(),
            content: message.to_string(),
            thread_id: None,
            timestamp,
            metadata: HashMap::new(),
        };

        match bus.publish_agent_message(&payload).await {
            Ok(_) => {
                let message_id = uuid::Uuid::new_v4().to_string();
                let response_json = serde_json::json!({
                    "message_id": message_id,
                    "status": "routed",
                    "target_agent_id": resolved_agent_id,
                    "message_type": message_type,
                    "delivery_method": "nats_to_acp",
                    "note": "Message published to NATS. Will be delivered to agent's ACP endpoint."
                });

                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&response_json).unwrap_or_default(),
                )]))
            }
            Err(e) => {
                error!("Failed to publish message to NATS: {}", e);
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Failed to route message: {}",
                    e
                ))]))
            }
        }
    }

    /// Register the agent's ACP HTTP endpoint so Ergatai can push tasks to it.
    /// Call this after connecting via MCP to enable bidirectional communication.
    #[tool(
        description = "Register the agent's ACP HTTP endpoint so Ergatai can push tasks to it"
    )]
    async fn set_acp_endpoint(
        &self,
        params: Parameters<SetAcpEndpointParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let endpoint = &params.0.endpoint;

        info!("🔧 set_acp_endpoint called with endpoint: {}", endpoint);

        // Validate endpoint URL
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Invalid endpoint URL. Must start with http:// or https://",
            )]));
        }

        // CRITICAL: Prevent agents from registering Ergatai's own endpoints
        // This would create a message loop where Ergatai sends messages to itself
        if endpoint.contains(self.ergatai_own_address.as_str()) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Invalid endpoint: Cannot register Ergatai's own address ({}) as ACP endpoint. \
                 Agents must run their own ACP server on a different port.",
                self.ergatai_own_address
            ))]));
        }

        // Get agent_id: use provided one, or fall back to session's registered agent_id
        let provided_agent_id = match &params.0.agent_id {
            Some(id) => id.clone(),
            None => {
                // Try to get from session
                match self.session_agent_id.read().await.clone() {
                    Some(id) => {
                        info!("No agent_id provided, using session agent_id: {}", id);
                        id
                    }
                    None => {
                        return Ok(CallToolResult::error(vec![ContentBlock::text(
                            "Missing agent_id parameter and no session agent_id available. \
                             Please provide your agent's ID.",
                        )]));
                    }
                }
            }
        };

        // Find the matching agent - support both exact ID and name prefix
        // (agents may not know their unique ID assigned during initialize)
        let agents = self.registry.list_agents().await;
        let matching_agent = agents
            .iter()
            .find(|a| {
                // Exact match
                a.agent_id == provided_agent_id
                // Or prefix match (e.g., "simple-agent" matches "simple-agent@ead00fad")
                || a.agent_id.starts_with(&format!("{}@", provided_agent_id))
            })
            .map(|a| a.agent_id.clone());

        let agent_id = match matching_agent {
            Some(id) => id,
            None => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Agent {} is not registered. Connect via MCP first.",
                    provided_agent_id
                ))]));
            }
        };

        // SECURITY: Track which agent is setting the endpoint
        // For now, we trust the agent_id parameter, but in production
        // we should validate against the MCP session's authenticated agent
        info!("✅ Agent {} registering ACP endpoint: {}", agent_id, endpoint);
        self.registry
            .set_acp_endpoint(&agent_id, endpoint.to_string())
            .await;

        // Verify it was stored
        let stored = self.registry.get_acp_endpoint(&agent_id).await;
        info!("✅ ACP endpoint stored in registry: {:?}", stored);

        let result = serde_json::json!({
            "agent_id": agent_id,
            "endpoint": endpoint,
            "status": "registered",
            "message": "ACP endpoint registered successfully. Ergatai can now push tasks to this agent."
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }

    /// Submit a DAG workflow for multi-agent collaboration
    #[tool(description = "Submit a DAG workflow for multi-agent collaboration")]
    async fn submit_orchestration(
        &self,
        params: Parameters<SubmitOrchestrationParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let dag_definition = &params.0.dag_definition;
        let _context = &params.0.context;

        info!("Submitting DAG orchestration ({} bytes)", dag_definition.len());

        // TODO: Integrate with actual DAG scheduler
        // The dag_definition contains the markdown-formatted DAG that should
        // be parsed and executed by DagScheduler
        let dag_id = uuid::Uuid::new_v4().to_string();

        let result = serde_json::json!({
            "dag_id": dag_id,
            "status": "submitted",
            "message": "DAG workflow submitted successfully (scheduler integration pending)",
            "note": "DAG definition received but execution not yet implemented"
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
        let dag_id = &params.0.dag_id;

        info!("Checking DAG status for {}", dag_id);

        // TODO: Query actual DAG scheduler
        let result = serde_json::json!({
            "dag_id": dag_id,
            "status": "running",
            "progress": {
                "total_nodes": 3,
                "completed_nodes": 1,
                "failed_nodes": 0
            },
            "results": {},
            "note": "Status query integration pending"
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        )]))
    }
}

// ── ServerHandler implementation ──

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ErgataiMcpServer {
    /// Handle initialize - auto-register the agent
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

        // Register agent in registry with unique ID
        self.registry
            .register_agent(unique_agent_id.clone(), connection_id.clone(), None, None)
            .await;

        info!("Agent registered: {} (connection: {})", unique_agent_id, connection_id);

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
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

/// Create the MCP Streamable HTTP service for mounting in axum.
///
/// Returns a `StreamableHttpService` that handles POST/GET/DELETE /mcp
/// with proper MCP 2025-06-18 protocol support.
///
/// # Arguments
/// * `registry` - Agent registry for tracking connected agents
/// * `cancellation_token` - Token for graceful shutdown
/// * `ergatai_own_address` - Ergatai's own address (e.g., "localhost:3000") for endpoint validation
pub fn create_mcp_service(
    registry: Arc<AgentRegistry>,
    cancellation_token: CancellationToken,
    ergatai_own_address: String,
) -> StreamableHttpService<ErgataiMcpServer, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(Some(std::time::Duration::from_secs(15)))
        .with_sse_retry(Some(std::time::Duration::from_secs(3)))
        .with_json_response(true)
        .with_cancellation_token(cancellation_token)
        .with_allowed_hosts([
            "localhost", "127.0.0.1", "::1", "0.0.0.0",
        ]);

    let own_address = Arc::new(ergatai_own_address);
    StreamableHttpService::new(
        move || Ok(ErgataiMcpServer::new(registry.clone(), own_address.clone())),
        Default::default(),
        config,
    )
}
