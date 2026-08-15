//! Simple ACP Agent Example
//!
//! Demonstrates a minimal ACP-compliant agent that can be used for testing.
//! Implements the ACP HTTP transport protocol (JSON-RPC over HTTP + SSE).
//!
//! # Usage
//!
//! ```bash
//! # Start Ergatai API server
//! cargo run --bin ergatai-api -- --port 3000
//!
//! # In another terminal, start this agent (port is auto-assigned)
//! cargo run -p simple-agent -- --agent-id my-agent --ergatai http://localhost:3000
//! ```

use std::sync::{Arc, OnceLock};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tracing::{error, info};
use uuid::Uuid;

/// Agent state shared across handlers
#[allow(dead_code)] // Example code demonstrating state structure
struct AgentState {
    agent_id: String,
    ergatai_endpoint: String,
    acp_port: u16,
    /// Active sessions
    sessions: Arc<tokio::sync::RwLock<std::collections::HashMap<String, SessionState>>>,
}

#[allow(dead_code)] // Example code demonstrating state structure
struct SessionState {
    session_id: String,
    cwd: String,
    created_at: std::time::Instant,
}

/// Global state
static AGENT_STATE: OnceLock<Arc<AgentState>> = OnceLock::new();

#[allow(dead_code)] // Example code demonstrating state access pattern
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

    info!("Starting ACP agent '{}'", agent_id);
    info!("Connecting to Ergatai at {}", ergatai_endpoint);

    // Step 1: Start ACP HTTP server on port 0 (OS assigns random available port)
    // Bind to port 0 first to get the actual port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let actual_port = listener.local_addr()?.port();

    info!(
        "ACP HTTP server listening on 127.0.0.1:{} (OS-assigned)",
        actual_port
    );

    // Initialize state with the actual port
    let state = Arc::new(AgentState {
        agent_id: agent_id.clone(),
        ergatai_endpoint: ergatai_endpoint.clone(),
        acp_port: actual_port,
        sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
    });

    if AGENT_STATE.set(state.clone()).is_err() {
        tracing::warn!("AGENT_STATE already initialized");
    }

    let app = Router::new()
        .route("/acp", post(handle_acp_request))
        .route("/health", get(health_check))
        .with_state(state.clone());

    // Start server in background
    let server_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("ACP HTTP server error: {}", e);
        }
    });

    // Step 2: Connect to Ergatai MCP and register with actual port
    let mcp_session = match connect_to_ergatai(&agent_id, &ergatai_endpoint, actual_port).await {
        Ok(session) => {
            info!("Agent is ready and registered with Ergatai!");
            Some(session)
        }
        Err(e) => {
            error!("Failed to connect to Ergatai: {}", e);
            None
        }
    };

    // Step 3: Start automatic conversation task
    let agent_id_clone = agent_id.clone();
    let ergatai_endpoint_clone = ergatai_endpoint.clone();
    tokio::spawn(async move {
        if let Some(session_id) = mcp_session {
            // Wait a bit for other agents to connect
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            // Start automatic conversation
            if let Err(e) = start_auto_conversation(&agent_id_clone, &ergatai_endpoint_clone, &session_id).await {
                error!("Auto conversation failed: {}", e);
            }
        }
    });

    // Wait for server to finish
    server_handle.await?;

    Ok(())
}

// ── ACP Protocol Handler ──

/// Handle all ACP JSON-RPC requests via POST /acp
async fn handle_acp_request(
    State(state): State<Arc<AgentState>>,
    headers: HeaderMap,
    Json(request): Json<serde_json::Value>,
) -> Response {
    // Extract connection ID and session ID from headers if present
    let connection_id = headers
        .get("Acp-Connection-Id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let method = request
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown");

    let id = request.get("id").cloned();

    info!(
        "ACP request: method={}, connection_id={:?}",
        method, connection_id
    );

    // Route to appropriate handler based on method
    let result = match method {
        "initialize" => handle_initialize(&request).await,
        "session/new" => handle_session_new(&state, &request).await,
        "session/prompt" => handle_session_prompt(&state, &request).await,
        "session/close" => handle_session_close(&state, &request).await,
        _ => {
            error!("Unknown ACP method: {}", method);
            Err(json!({
                "code": -32601,
                "message": format!("Method not found: {}", method)
            }))
        }
    };

    // Build JSON-RPC response
    let response = match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": error
        }),
    };

    // Return with appropriate headers
    let mut response_headers = HeaderMap::new();
    response_headers.insert("Content-Type", "application/json".parse().unwrap());
    if let Some(conn_id) = connection_id {
        response_headers.insert("Acp-Connection-Id", conn_id.parse().unwrap());
    }

    (StatusCode::OK, response_headers, Json(response)).into_response()
}

/// Handle ACP initialize
async fn handle_initialize(_request: &serde_json::Value) -> Result<serde_json::Value, serde_json::Value> {
    info!("ACP initialize request");

    Ok(json!({
        "protocolVersion": "2025-11-25",
        "serverInfo": {
            "name": "simple-acp-agent",
            "version": "0.1.0"
        },
        "capabilities": {
            "sessions": {}
        }
    }))
}

/// Handle session/new
async fn handle_session_new(
    state: &AgentState,
    request: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let params = request.get("params").ok_or_else(|| json!({
        "code": -32602,
        "message": "Missing params"
    }))?;

    let cwd = params
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("/tmp");

    let session_id = Uuid::new_v4().to_string();

    info!("Creating ACP session {} (cwd: {})", session_id, cwd);

    // Store session
    let session_state = SessionState {
        session_id: session_id.clone(),
        cwd: cwd.to_string(),
        created_at: std::time::Instant::now(),
    };

    state.sessions.write().await.insert(session_id.clone(), session_state);

    Ok(json!({
        "sessionId": session_id
    }))
}

/// Handle session/prompt
async fn handle_session_prompt(
    state: &AgentState,
    request: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let params = request.get("params").ok_or_else(|| json!({
        "code": -32602,
        "message": "Missing params"
    }))?;

    let session_id = params
        .get("sessionId")
        .and_then(|s| s.as_str())
        .ok_or_else(|| json!({
            "code": -32602,
            "message": "Missing sessionId"
        }))?;

    let prompt = params.get("prompt").ok_or_else(|| json!({
        "code": -32602,
        "message": "Missing prompt"
    }))?;

    info!("Received prompt for session {}", session_id);

    // Check if session exists
    if !state.sessions.read().await.contains_key(session_id) {
        return Err(json!({
            "code": -32000,
            "message": format!("Session not found: {}", session_id)
        }));
    }

    // Extract user message (simplified - just get text content)
    let user_message = prompt
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|content| content.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("(no message)");

    info!("User message: {}", user_message);

    // Simulate processing
    let response_text = format!(
        "Hello from {}! I received: '{}'",
        state.agent_id, user_message
    );

    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": response_text
            }
        ]
    }))
}

/// Handle session/close
async fn handle_session_close(
    state: &AgentState,
    request: &serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let params = request.get("params").ok_or_else(|| json!({
        "code": -32602,
        "message": "Missing params"
    }))?;

    let session_id = params
        .get("sessionId")
        .and_then(|s| s.as_str())
        .ok_or_else(|| json!({
            "code": -32602,
            "message": "Missing sessionId"
        }))?;

    info!("Closing ACP session {}", session_id);

    state.sessions.write().await.remove(session_id);

    Ok(json!({}))
}

// ── Health Check ──

async fn health_check() -> &'static str {
    "OK"
}

// ── MCP Client (Connect to Ergatai) ──

/// Parse SSE response and extract JSON from data: lines
fn parse_sse_response(body: &str) -> anyhow::Result<serde_json::Value> {
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
            if let Ok(json) = serde_json::from_str(line) {
                return Ok(json);
            }
        }
    }
    serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {} - body: {}", e, body))
}

/// Connect to Ergatai's MCP endpoint and register this agent
async fn connect_to_ergatai(
    agent_id: &str,
    ergatai_endpoint: &str,
    acp_port: u16,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let mcp_url = format!("{}/mcp", ergatai_endpoint);

    // Initialize with MCP protocol, including ACP port in meta
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "clientInfo": {
                "name": agent_id,
                "version": "0.1.0"
            },
            "capabilities": {},
            "_meta": {
                "acp_port": acp_port
            }
        }
    });

    info!(
        "Sending MCP initialize to {} with ACP port {}",
        mcp_url, acp_port
    );

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&init_request)
        .send()
        .await?;

    let status = response.status();
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

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

    // Send initialized notification
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });

    let _ = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&notification)
        .send()
        .await?;

    info!("Sent initialized notification");

    // Auto-register ACP endpoint using the tool
    let acp_endpoint = format!("http://127.0.0.1:{}", acp_port);
    let register_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "set_acp_endpoint",
            "arguments": {
                "agent_id": agent_id,  // MUST provide agent_id
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
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&register_request)
        .send()
        .await?;

    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;

    if body.get("error").is_some() {
        error!("Failed to register ACP endpoint: {:?}", body);
        return Err(anyhow::anyhow!("Failed to register ACP endpoint"));
    }

    info!("✅ ACP endpoint registered successfully");

    // List available tools
    let list_tools_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&list_tools_request)
        .send()
        .await?;

    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;
    info!("Available Ergatai tools: {:?}", body.get("result"));

    Ok(session_id)
}

/// Start automatic conversation with other agents
async fn start_auto_conversation(
    agent_id: &str,
    ergatai_endpoint: &str,
    mcp_session_id: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let mcp_url = format!("{}/mcp", ergatai_endpoint);

    info!("🤖 Starting automatic conversation...");

    // Step 1: List other agents
    let list_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "list_agents",
            "arguments": {}
        }
    });

    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", mcp_session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&list_request)
        .send()
        .await?;

    let body_text = response.text().await?;
    let body = parse_sse_response(&body_text)?;

    // Parse agent list
    let agents_json = body
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("{}");

    let agents_data: serde_json::Value = serde_json::from_str(agents_json)?;
    let agents = agents_data
        .get("agents")
        .and_then(|a| a.as_array())
        .unwrap_or(&vec![])
        .clone();

    info!("Found {} agents in the system", agents.len());

    // Find other agents (not this one)
    let other_agents: Vec<_> = agents
        .iter()
        .filter(|a| {
            a.get("agent_id")
                .and_then(|id| id.as_str())
                .map(|id| !id.starts_with(agent_id))
                .unwrap_or(false)
        })
        .collect();

    if other_agents.is_empty() {
        info!("No other agents to talk to. Waiting for more agents to join...");
        return Ok(());
    }

    info!("Found {} other agents to chat with", other_agents.len());

    // Step 2: Send message to the first other agent
    if let Some(target_agent) = other_agents.first() {
        let target_id = target_agent
            .get("agent_id")
            .and_then(|id| id.as_str())
            .unwrap_or("unknown");

        info!("💬 Sending message to {}...", target_id);

        let message = format!(
            "Hi from {}! Let's collaborate. What are you working on?",
            agent_id
        );

        let send_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "send_message",
                "arguments": {
                    "target_agent_id": target_id,
                    "message": message,
                    "message_type": "request"
                }
            }
        });

        let response = client
            .post(&mcp_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", mcp_session_id)
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&send_request)
            .send()
            .await?;

        let body_text = response.text().await?;
        let body = parse_sse_response(&body_text)?;

        if let Some(result) = body.get("result") {
            info!("✅ Message sent successfully: {:?}", result);
        } else if let Some(error) = body.get("error") {
            error!("❌ Failed to send message: {:?}", error);
        }
    }

    // Step 3: Wait and listen for responses
    info!("👂 Listening for incoming messages...");

    // Keep the agent running to receive messages
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        info!("Agent still active, waiting for more conversations...");
    }
}
