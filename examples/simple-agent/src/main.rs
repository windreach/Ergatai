//! Simple Agent Example
//!
//! Demonstrates how an agent works in the middleware architecture:
//! 1. Agent starts an HTTP server exposing ACP endpoints
//! 2. Agent connects to Ergatai's MCP endpoint (2025-03-26 protocol)
//! 3. Agent registers its ACP endpoint via `set_acp_endpoint` tool
//! 4. Ergatai can now push tasks to the agent via ACP HTTP
//!
//! # Usage
//!
//! ```bash
//! # Start Ergatai API server
//! cargo run -p ergatai-api -- --port 3000
//!
//! # In another terminal, start this agent
//! cargo run -p simple-agent -- --port 8080 --agent-id my-agent --ergatai http://localhost:3000
//! ```

use std::sync::{Arc, OnceLock};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

/// Agent state shared across handlers
struct AgentState {
    agent_id: String,
    /// Current session ID (if any)
    session_id: Option<String>,
    /// Ergatai MCP endpoint
    ergatai_endpoint: String,
}

/// Global state
static AGENT_STATE: OnceLock<Arc<AgentState>> = OnceLock::new();

fn get_state() -> Arc<AgentState> {
    AGENT_STATE
        .get()
        .expect("AGENT_STATE not initialized")
        .clone()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080u16);

    let agent_id = args
        .iter()
        .position(|a| a == "--agent-id")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "simple-agent".to_string());

    let ergatai_endpoint = args
        .iter()
        .position(|a| a == "--ergatai")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("Starting agent '{}' on port {}", agent_id, port);
    info!("Connecting to Ergatai at {}", ergatai_endpoint);

    // Initialize state
    let state = Arc::new(AgentState {
        agent_id: agent_id.clone(),
        session_id: None,
        ergatai_endpoint: ergatai_endpoint.clone(),
    });
    if AGENT_STATE.set(state.clone()).is_err() {
        tracing::warn!("AGENT_STATE already initialized (this should not happen)");
    }

    // Step 1: Connect to Ergatai MCP and register
    if let Err(e) = connect_to_ergatai(&agent_id, &ergatai_endpoint, port).await {
        error!("Failed to connect to Ergatai: {}", e);
        // Continue anyway - agent can still serve ACP requests
    }

    // Step 2: Start ACP HTTP server
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/acp/session/new", post(create_session))
        .route("/acp/session/{id}/prompt", post(handle_prompt))
        .route("/acp/session/{id}/close", post(close_session))
        .with_state(state.clone());

    let addr = format!("0.0.0.0:{}", port);
    info!("ACP HTTP server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Parse SSE response and extract JSON from data: lines
fn parse_sse_response(body: &str) -> anyhow::Result<serde_json::Value> {
    // SSE format: "data: {...}\n\n" or just "{...}" for JSON mode
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("data: ") {
            let json_str = &line[6..];
            if !json_str.is_empty() {
                if let Ok(json) = serde_json::from_str(json_str) {
                    return Ok(json);
                }
            }
        } else if line.starts_with('{') {
            // Direct JSON response (when server uses json_response mode)
            if let Ok(json) = serde_json::from_str(line) {
                return Ok(json);
            }
        }
    }
    // If no data: lines found, try parsing the whole body as JSON
    serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {} - body: {}", e, body))
}

/// Connect to Ergatai's MCP endpoint and register this agent.
/// Uses MCP 2025-03-26 protocol with Streamable HTTP transport.
async fn connect_to_ergatai(
    agent_id: &str,
    ergatai_endpoint: &str,
    port: u16,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let mcp_url = format!("{}/mcp", ergatai_endpoint);

    // Step 1: Initialize with MCP 2025-03-26 protocol (camelCase fields)
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "clientInfo": {
                "name": agent_id,
                "version": "0.1.0"
            },
            "capabilities": {}
        }
    });

    info!("Sending MCP initialize to {}", mcp_url);

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&init_request)
        .send()
        .await?;

    let status = response.status();

    // Extract session ID from response headers
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Parse SSE response
    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;

    if !status.is_success() {
        error!("MCP initialize failed: {} - {:?}", status, body);
        return Err(anyhow::anyhow!("MCP initialize failed: {}", status));
    }

    info!(
        "MCP initialize successful, session: {}, server: {:?}",
        session_id,
        body.get("result")
            .and_then(|r| r.get("serverInfo"))
            .unwrap_or(&serde_json::Value::Null)
    );

    // Step 2: Send initialized notification
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });

    let _ = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-03-26")
        .json(&notification)
        .send()
        .await?;

    info!("Sent initialized notification");

    // Step 3: Register ACP endpoint via set_acp_endpoint tool
    let acp_endpoint = format!("http://localhost:{}", port);
    let tool_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "set_acp_endpoint",
            "arguments": {
                "agent_id": agent_id,
                "endpoint": acp_endpoint
            }
        }
    });

    info!("Registering ACP endpoint: {}", acp_endpoint);

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-03-26")
        .json(&tool_request)
        .send()
        .await?;

    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;

    if body.get("error").is_some() {
        error!("set_acp_endpoint failed: {:?}", body);
        return Err(anyhow::anyhow!("set_acp_endpoint failed"));
    }

    info!("ACP endpoint registered successfully");

    // Step 4: List available tools (for demonstration)
    let list_tools_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list",
        "params": {}
    });

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-03-26")
        .json(&list_tools_request)
        .send()
        .await?;

    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;
    info!("Available Ergatai tools: {:?}", body.get("result"));

    Ok(())
}

// ── ACP HTTP Handlers ──

async fn health_check() -> &'static str {
    "OK"
}

#[derive(Debug, Deserialize)]
struct NewSessionRequest {
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Serialize)]
struct NewSessionResponse {
    session_id: String,
}

async fn create_session(Json(req): Json<NewSessionRequest>) -> impl IntoResponse {
    let state = get_state();
    let session_id = uuid::Uuid::new_v4().to_string();

    info!(
        "Creating session {} for agent {} (cwd: {:?})",
        session_id, state.agent_id, req.cwd
    );

    (StatusCode::OK, Json(NewSessionResponse { session_id }))
}

#[derive(Debug, Deserialize)]
struct PromptRequest {
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct PromptResponse {
    content: Vec<Content>,
}

#[derive(Debug, Serialize)]
struct Content {
    r#type: String,
    text: String,
}

async fn handle_prompt(
    axum::extract::Path(session_id): axum::extract::Path<String>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let state = get_state();

    info!(
        "Received prompt for session {} (agent: {})",
        session_id, state.agent_id
    );

    // Extract the last user message
    let user_message = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("(no message)");

    info!("User message: {}", user_message);

    // Simulate processing and response
    let response_text = format!(
        "Hello from {}! I received your message: '{}'",
        state.agent_id, user_message
    );

    let response = PromptResponse {
        content: vec![Content {
            r#type: "text".to_string(),
            text: response_text,
        }],
    };

    (StatusCode::OK, Json(response))
}

#[derive(Debug, Serialize)]
struct CloseSessionResponse {
    status: String,
}

async fn close_session(
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    info!("Closing session {}", session_id);

    (
        StatusCode::OK,
        Json(CloseSessionResponse {
            status: "closed".to_string(),
        }),
    )
}
