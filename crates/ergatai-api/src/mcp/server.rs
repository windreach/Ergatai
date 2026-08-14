//! MCP Server implementation
//!
//! Handles MCP protocol over HTTP/WebSocket

use std::sync::Arc;
use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tracing::{error, info};

use super::agent_registry::AgentRegistry;
use super::tools;
use super::types::*;

/// MCP Server state
#[derive(Clone)]
pub struct McpServer {
    registry: Arc<AgentRegistry>,
    server_info: ServerInfo,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new() -> Self {
        Self {
            registry: Arc::new(AgentRegistry::new()),
            server_info: ServerInfo {
                name: "ergatai".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        }
    }

    /// Get the agent registry
    pub fn registry(&self) -> &AgentRegistry {
        &self.registry
    }

    /// Create Axum router for MCP server
    /// Takes AppState to match the main router's state type
    pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
        let state = Arc::new(self);

        Router::new()
            .route("/mcp", post(handle_mcp_request))
            .route("/mcp/ws", get(ws_handler))
            .with_state(state)
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle MCP JSON-RPC requests over HTTP
async fn handle_mcp_request(
    State(server): State<Arc<McpServer>>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    info!("MCP request: method={}", request.method);

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(&server, request).await,
        "tools/list" => handle_tools_list(&server, request).await,
        "tools/call" => handle_tools_call(&server, request).await,
        "ping" => handle_ping(request).await,
        _ => {
            error!("Unknown MCP method: {}", request.method);
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                }),
            }
        }
    };

    Json(response)
}

/// Handle initialize request - Auto-register agent on connection
async fn handle_initialize(
    server: &Arc<McpServer>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    // Parse initialize request
    let init_request: Result<InitializeRequest, _> = serde_json::from_value(request.params.clone());

    match init_request {
        Ok(req) => {
            // Extract agent info from client_info
            let agent_id = req.client_info.name.clone();
            let agent_version = req.client_info.version.clone();

            info!(
                "Agent connecting: {} (version: {}, protocol: {})",
                agent_id, agent_version, req.protocol_version
            );

            // Generate a unique connection ID
            let connection_id = uuid::Uuid::new_v4().to_string();

            // Register agent in registry
            server.registry.register_agent(
                agent_id.clone(),
                connection_id.clone(),
                None, // capabilities will be updated later if needed
            ).await;

            info!("Agent registered: {} (connection_id: {})", agent_id, connection_id);

            // Return initialize response
            let response = InitializeResponse {
                protocol_version: "2024-11-05".to_string(),
                server_info: server.server_info.clone(),
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability {
                        list_changed: false,
                    }),
                },
            };

            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(serde_json::to_value(response).unwrap()),
                error: None,
            }
        }
        Err(e) => {
            error!("Failed to parse initialize request: {}", e);
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: format!("Invalid initialize params: {}", e),
                    data: None,
                }),
            }
        }
    }
}

/// Handle tools/list request
async fn handle_tools_list(
    _server: &Arc<McpServer>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let tools = tools::get_tool_definitions();
    let response = ToolsListResponse { tools };

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: request.id,
        result: Some(serde_json::to_value(response).unwrap()),
        error: None,
    }
}

/// Handle tools/call request
async fn handle_tools_call(
    server: &Arc<McpServer>,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let tool_call: Result<ToolCallRequest, _> = serde_json::from_value(request.params.clone());

    match tool_call {
        Ok(call) => {
            let result = tools::handle_tool_call(&call.name, call.arguments, &server.registry).await;

            match result {
                Ok(response) => JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: Some(serde_json::to_value(response).unwrap()),
                    error: None,
                },
                Err(e) => {
                    error!("Tool call error: {}", e);
                    JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: request.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: e.to_string(),
                            data: None,
                        }),
                    }
                }
            }
        }
        Err(e) => {
            error!("Invalid tool call request: {}", e);
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: format!("Invalid params: {}", e),
                    data: None,
                }),
            }
        }
    }
}

/// Handle ping request
async fn handle_ping(request: JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: request.id,
        result: Some(json!({})),
        error: None,
    }
}

/// WebSocket handler (for future use)
async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    info!("WebSocket upgrade requested");
    // TODO: Implement WebSocket MCP handler
    ws.on_upgrade(|_socket| async {
        // WebSocket connection handling
    })
}
