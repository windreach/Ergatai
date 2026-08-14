//! Ergatai API Server - HTTP/WebSocket API for Ergatai
//!
//! This server provides a REST API and WebSocket interface for interacting
//! with Ergatai's multi-agent collaboration features.
//!
//! # Usage
//!
//! ```bash
//! ergatai-api --port 3000
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use ergatai_core::acp::manager::{self as acp_manager, SessionKind};
// TODO(middleware): Re-enable after HTTP client migration
// use ergatai_core::acp::sdk_session::spawn_session_task_with_kind;
// use ergatai_core::agent::config::{get_agent_config, AgentConfig};
// use ergatai_core::agent::discovery::discover_acp_runtimes;
// use ergatai_core::agent::hosted_config::list_hosted_agents;
use ergatai_core::cross_agent::{get_dag_scheduler, DagScheduler};
use ergatai_core::nats;

// MCP module
mod mcp;
use mcp::{create_mcp_service, start_nats_acp_forwarder};

/// Shared application state available to all handlers.
#[derive(Clone)]
struct AppState {
    /// Default working directory for new chat sessions when the request does not
    /// provide one. Falls back to the process cwd.
    default_cwd: String,
    /// Optional API token for authentication. If set, all requests must include
    /// an `Authorization: Bearer <token>` header.
    api_token: Option<String>,
}

static APP_STATE: OnceLock<AppState> = OnceLock::new();

fn app_state_with_token(token: Option<String>) -> &'static AppState {
    APP_STATE.get_or_init(|| AppState {
        default_cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string()),
        api_token: token,
    })
}

#[derive(Parser)]
#[command(name = "ergatai-api")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "3000")]
    port: u16,

    /// Host to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// API token for authentication. If not provided, API is open to all local clients.
    /// Can also be set via ERGATAI_API_TOKEN environment variable.
    #[arg(long, env = "ERGATAI_API_TOKEN")]
    api_token: Option<String>,
}

/// Parse arguments and set environment variables BEFORE the tokio runtime starts.
/// This is critical: std::env::set_var is unsafe to call after threads are spawned.
fn setup_env_before_runtime() -> Args {
    let args = Args::parse();

    // Set RUST_LOG before any threads are spawned
    if args.verbose {
        // Safety: This is called from main() BEFORE tokio runtime starts.
        // At this point, only the main thread exists, so no data race is possible.
        // tracing_subscriber reads RUST_LOG once during init_logging() later.
        unsafe { std::env::set_var("RUST_LOG", "debug") };
    }

    args
}

fn main() -> Result<()> {
    // Parse args and setup env BEFORE tokio runtime (for safe set_var)
    let args = setup_env_before_runtime();

    // Now start the tokio runtime and run the async main
    tokio::runtime::Runtime::new()?.block_on(async_main(args))
}

async fn async_main(args: Args) -> Result<()> {
    ergatai_core::init_logging();
    ergatai_core::init_panic_hook();

    // Install OS signal handlers (SIGINT/SIGTERM) so child processes
    // (NATS, MCP, ACP sessions) are cleaned up gracefully on Ctrl+C.
    if let Err(e) = ergatai_core::setup_signal_handlers().await {
        eprintln!("Warning: failed to install signal handlers: {}", e);
    }

    tracing::info!("Starting Ergatai API server on {}:{}", args.host, args.port);

    if args.api_token.is_some() {
        tracing::info!("API authentication enabled");
    } else {
        tracing::warn!("API authentication disabled - API is open to all local clients");
    }

    // Initialize MCP server with Streamable HTTP transport
    let mcp_registry = std::sync::Arc::new(mcp::AgentRegistry::new());
    let mcp_cancellation_token = CancellationToken::new();
    let ergatai_own_address = format!("{}:{}", args.host, args.port);
    let mcp_service = create_mcp_service(
        mcp_registry.clone(),
        mcp_cancellation_token.clone(),
        ergatai_own_address,
    );
    tracing::info!("MCP server initialized (protocol 2025-06-18, Streamable HTTP)");

    // Start NATS → ACP message forwarder
    start_nats_acp_forwarder(mcp_registry.clone(), mcp_cancellation_token.clone());
    tracing::info!("NATS → ACP message forwarder started");

    // Build application router
    let state = app_state_with_token(args.api_token.clone()).clone();
    let app = Router::new()
        // REST API routes
        .route("/health", get(health_check))
        // TODO(middleware): Re-enable after HTTP client migration
        // .route("/api/v1/chat", post(create_chat))
        // .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/dag", post(submit_dag))
        .route("/api/v1/dag/status", get(dag_status))
        // MCP Streamable HTTP endpoint (POST/GET/DELETE /mcp)
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    // Start server
    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --host '{}': {}", args.host, e))?;

    tracing::info!("API server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> &'static str {
    "OK"
}

// ── Request / response types ──

#[derive(Debug, Deserialize)]
struct CreateChatRequest {
    /// Agent name to start the session with (e.g. "claude", "codex", or a hosted agent name).
    agent: String,
    /// Optional working directory for the session. Falls back to the server default cwd.
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateChatResponse {
    session_id: String,
    agent: String,
    cwd: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize)]
struct AgentSummary {
    name: String,
    display_name: Option<String>,
    source: String, // "hosted" | "builtin"
    available: bool,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    nats_initialized: bool,
    nats_port: Option<u16>,
    sessions: Vec<SessionInfo>,
}

#[derive(Debug, Serialize)]
struct SessionInfo {
    session_id: String,
    agent_name: String,
    cwd: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct DagStatusResponse {
    running: bool,
    progress: Option<f32>,
    status_prompt: Option<String>,
    is_complete: Option<bool>,
}

// ── Middleware ──

/// Authentication middleware. If an API token is configured, requires all
/// requests to include a valid `Authorization: Bearer <token>` header.
/// Health check endpoint is exempt from authentication.
async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    // Health check is always public
    if request.uri().path() == "/health" {
        return next.run(request).await.into_response();
    }

    // If no token is configured, allow all requests
    let Some(expected_token) = &state.api_token else {
        return next.run(request).await.into_response();
    };

    // Extract and validate bearer token
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    let is_valid = auth_header
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|token| token == expected_token.as_str())
        .unwrap_or(false);

    if !is_valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid or missing API token. Provide via Authorization: Bearer <token>"
                    .to_string(),
            }),
        )
            .into_response();
    }

    next.run(request).await.into_response()
}

/// Validate and canonicalize a working directory path. Rejects paths that:
/// - Don't exist or aren't directories
/// - Contain path traversal attempts (..)
/// - Are absolute paths outside the allowed base (if configured)
fn validate_cwd(cwd: &str) -> Result<PathBuf, String> {
    let path = Path::new(cwd);

    // Reject paths containing .. components to prevent traversal
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err("Path traversal (..) not allowed in cwd".to_string());
        }
    }

    // Canonicalize to resolve symlinks and relative paths
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Invalid cwd '{}': {}", cwd, e))?;

    // Ensure it's a directory
    if !canonical.is_dir() {
        return Err(format!("cwd '{}' is not a directory", cwd));
    }

    Ok(canonical)
}

// ── Handlers ──

// TODO(middleware): Re-enable after HTTP client migration
// POST /api/v1/chat and GET /api/v1/agents are disabled in middleware mode.
// Agents now connect via MCP and are tracked in AgentRegistry.

/// GET /api/v1/status — system status snapshot.
///
/// Returns NATS connectivity status and the list of active ACP sessions.
async fn get_status() -> impl IntoResponse {
    let nats_initialized = nats::is_nats_initialized().await;
    let nats_port = nats::get_nats_server_port().await;

    let sessions: Vec<SessionInfo> = acp_manager::manager()
        .list_sessions()
        .await
        .into_iter()
        .map(|s| SessionInfo {
            session_id: s.session_id,
            agent_name: s.agent_name,
            cwd: s.cwd,
            status: s.status,
        })
        .collect();

    Json(StatusResponse {
        nats_initialized,
        nats_port,
        sessions,
    })
}

/// POST /api/v1/dag — placeholder for DAG submission.
///
/// A full implementation would parse the markdown body, build a `TaskGraph`,
/// and set a `DagScheduler`. For now we return a 501 so clients can detect
/// that the endpoint exists but is not yet wired up.
async fn submit_dag() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse {
            error: "DAG submission is not yet implemented".to_string(),
        }),
    )
}

/// GET /api/v1/dag/status — DAG scheduler status.
///
/// Returns whether a DAG is currently running and, if so, its progress.
async fn dag_status() -> impl IntoResponse {
    let scheduler: Option<DagScheduler> = get_dag_scheduler();

    let Some(scheduler) = scheduler else {
        return Json(DagStatusResponse {
            running: false,
            progress: None,
            status_prompt: None,
            is_complete: None,
        });
    };

    let progress = scheduler.progress().await;
    let status_prompt = scheduler.status_prompt().await;
    let is_complete = scheduler.is_complete().await;

    Json(DagStatusResponse {
        running: true,
        progress: Some(progress),
        status_prompt: Some(status_prompt),
        is_complete: Some(is_complete),
    })
}
