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
use tracing::{error, info};

use ergatai_core::acp::manager::{self as acp_manager, SessionKind};
use ergatai_core::acp::sdk_session::spawn_session_task_with_kind;
use ergatai_core::agent::config::{get_agent_config, AgentConfig};
use ergatai_core::agent::discovery::discover_acp_runtimes;
use ergatai_core::agent::hosted_config::list_hosted_agents;
use ergatai_core::cross_agent::{get_dag_scheduler, DagScheduler};
use ergatai_core::nats;

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
    #[arg(short, long, default_value = "127.0.0.1")]
    host: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// API token for authentication. If not provided, API is open to all local clients.
    /// Can also be set via ERGATAI_API_TOKEN environment variable.
    #[arg(long, env = "ERGATAI_API_TOKEN")]
    api_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    if args.verbose {
        // Safety: set_var is called before any threads are spawned
        unsafe { std::env::set_var("RUST_LOG", "debug") };
    }

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

    // Build application router
    let state = app_state_with_token(args.api_token.clone()).clone();
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/api/v1/chat", post(create_chat))
        .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/dag", post(submit_dag))
        .route("/api/v1/dag/status", get(dag_status))
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

/// POST /api/v1/chat — create a new ACP chat session.
///
/// Spawns an ACP session task for the requested agent and returns the
/// `session_id` as soon as the session has been registered with the
/// global session manager.
async fn create_chat(
    State(state): State<AppState>,
    Json(req): Json<CreateChatRequest>,
) -> impl IntoResponse {
    // Validate and canonicalize the working directory
    let cwd = match req.cwd.as_deref() {
        Some(cwd_str) => match validate_cwd(cwd_str) {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(e) => {
                let body = Json(ErrorResponse { error: e });
                return (StatusCode::BAD_REQUEST, body).into_response();
            }
        },
        None => state.default_cwd.clone(),
    };

    // Resolve the agent config. This supports both built-in agents
    // (legacy `~/.config/ergatai/agents/{name}.json`) and hosted agents
    // (`~/.config/ergatai/agents/{name}/settings.json`).
    let config: AgentConfig = match get_agent_config(&req.agent) {
        Ok(cfg) => cfg,
        Err(e) => {
            let body = Json(ErrorResponse {
                error: format!("Agent '{}' not found: {}", req.agent, e),
            });
            return (StatusCode::NOT_FOUND, body).into_response();
        }
    };

    // Channel that receives the session_id once the session task has
    // registered with the global session manager.
    let (session_id_tx, session_id_rx) = oneshot::channel();

    spawn_session_task_with_kind(config, cwd.clone(), SessionKind::Chat, session_id_tx);

    // Wait for the session id with a bounded timeout — the ACP handshake
    // can take a few seconds on a cold start.
    let session_id =
        match tokio::time::timeout(std::time::Duration::from_secs(30), session_id_rx).await {
            Ok(Ok(Ok(id))) => id,
            Ok(Ok(Err(e))) => {
                error!("Failed to start ACP session: {}", e);
                let body = Json(ErrorResponse {
                    error: format!("Failed to start session: {}", e),
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, body).into_response();
            }
            Ok(Err(_)) => {
                error!("Session id sender dropped before returning session_id");
                let body = Json(ErrorResponse {
                    error: "Session task terminated unexpectedly".to_string(),
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, body).into_response();
            }
            Err(_) => {
                error!("Timed out waiting for ACP session to initialize");
                let body = Json(ErrorResponse {
                    error: "Timed out waiting for session to start".to_string(),
                });
                return (StatusCode::GATEWAY_TIMEOUT, body).into_response();
            }
        };

    info!(session_id = %session_id, agent = %req.agent, "Created chat session");

    let resp = CreateChatResponse {
        session_id,
        agent: req.agent,
        cwd,
    };
    (StatusCode::CREATED, Json(resp)).into_response()
}

/// GET /api/v1/agents — list available agents.
///
/// Combines:
/// - Hosted agents from `~/.config/ergatai/agents/{name}/settings.json`
/// - Built-in ACP runtimes discovered via `discover_acp_runtimes`
async fn list_agents() -> impl IntoResponse {
    let mut agents: Vec<AgentSummary> = Vec::new();

    // Hosted (user-created) agents.
    match list_hosted_agents() {
        Ok(names) => {
            for name in names {
                // Attempt to load the config to pull the display_name; if it
                // fails we still include the entry but mark it unavailable.
                let (display_name, available) = match get_agent_config(&name) {
                    Ok(cfg) => (cfg.display_name, true),
                    Err(_) => (None, false),
                };
                agents.push(AgentSummary {
                    name,
                    display_name,
                    source: "hosted".to_string(),
                    available,
                });
            }
        }
        Err(e) => {
            tracing::warn!("Failed to list hosted agents: {}", e);
        }
    }

    // Built-in ACP runtimes (claude, codex, goose, etc.).
    for entry in discover_acp_runtimes() {
        // Skip runtimes that already have a hosted counterpart — the
        // hosted variant takes precedence and is more specific.
        if agents.iter().any(|a| a.name == entry.id) {
            continue;
        }
        let available = matches!(
            entry.availability,
            ergatai_core::agent::discovery::AcpAvailabilityStatus::Available
        );
        agents.push(AgentSummary {
            name: entry.id,
            display_name: Some(entry.label),
            source: "builtin".to_string(),
            available,
        });
    }

    agents.sort_by(|a, b| a.name.cmp(&b.name));
    Json(agents)
}

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
