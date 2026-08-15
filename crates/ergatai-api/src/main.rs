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
use tokio_util::sync::CancellationToken;
use tower_governor::{
    GovernorLayer,
    governor::GovernorConfigBuilder,
    key_extractor::KeyExtractor,
};
use ergatai_core::cross_agent::{get_dag_scheduler, set_dag_scheduler, DagScheduler};
use ergatai_core::nats;

// MCP module
mod mcp;
use mcp::{create_mcp_service, start_peer_reaper};

/// Shared application state available to all handlers.
#[derive(Clone)]
struct AppState {
    /// Default working directory for new chat sessions when the request does not
    /// provide one. Falls back to the process cwd.
    #[allow(dead_code)] // Reserved for future chat session management
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

/// Prometheus metrics handle, stored globally for /metrics endpoint access.
static PROMETHEUS_HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

/// Custom key extractor for rate limiting by Agent ID.
///
/// Extracts agent identifier from MCP session header or falls back to IP address.
/// This prevents individual agents from spamming while allowing normal conversation flow.
#[derive(Clone)]
struct AgentKeyExtractor;

impl KeyExtractor for AgentKeyExtractor {
    type Key = String;

    fn extract<B>(&self, req: &Request<B>) -> Result<Self::Key, tower_governor::errors::GovernorError> {
        // Prefer per-session key for MCP traffic — each MCP session gets its own bucket.
        if let Some(session_id) = req.headers().get("mcp-session-id") {
            if let Ok(session_str) = session_id.to_str() {
                return Ok(format!("agent:{}", session_str));
            }
        }

        // Fall back to peer IP for non-MCP requests. axum only populates
        // `ConnectInfo<SocketAddr>` when the server is built with
        // `into_make_service_with_connect_info::<SocketAddr>()` — see router
        // setup below. If the extension is missing, bucket under a shared key
        // so all anonymous traffic is collectively rate-limited (not exempt).
        if let Some(connect_info) = req.extensions().get::<axum::extract::ConnectInfo<SocketAddr>>() {
            return Ok(format!("ip:{}", connect_info.0.ip()));
        }
        if let Some(peer_addr) = req.extensions().get::<SocketAddr>() {
            return Ok(format!("ip:{}", peer_addr.ip()));
        }

        // Last resort: shared bucket for anonymous / unidentifiable requests.
        Ok("anonymous".to_string())
    }
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

    /// TLS certificate file (PEM format) for HTTPS support.
    /// Can also be set via ERGATAI_TLS_CERT environment variable.
    #[arg(long, env = "ERGATAI_TLS_CERT")]
    tls_cert: Option<PathBuf>,

    /// TLS private key file (PEM format) for HTTPS support.
    /// Can also be set via ERGATAI_TLS_KEY environment variable.
    #[arg(long, env = "ERGATAI_TLS_KEY")]
    tls_key: Option<PathBuf>,

    /// SSE keep-alive interval in seconds. Lower values detect dead clients faster
    /// but increase network traffic. Default: 15.
    /// Can also be set via ERGATAI_SSE_KEEP_ALIVE environment variable.
    #[arg(long, env = "ERGATAI_SSE_KEEP_ALIVE", default_value = "15")]
    sse_keep_alive: u64,
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

    // Initialize Prometheus metrics exporter
    let prometheus_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("Failed to install Prometheus recorder: {}", e))?;
    let _ = PROMETHEUS_HANDLE.set(prometheus_handle);
    tracing::info!("Prometheus metrics exporter initialized");

    // Install OS signal handlers (SIGINT/SIGTERM) so child processes
    // (NATS, tmux panes) are cleaned up gracefully on Ctrl+C.
    if let Err(e) = ergatai_core::setup_signal_handlers().await {
        eprintln!("Warning: failed to install signal handlers: {}", e);
    }

    tracing::info!("Starting Ergatai API server on {}:{}", args.host, args.port);

    // Authentication is optional - only enabled if --api-token is provided
    if args.api_token.is_some() {
        tracing::info!("API authentication enabled");
    } else {
        tracing::info!("API authentication disabled - API is open to all clients");
    }

    // Validate TLS configuration
    let tls_enabled = args.tls_cert.is_some() || args.tls_key.is_some();
    if tls_enabled {
        match (&args.tls_cert, &args.tls_key) {
            (Some(cert), Some(key)) => {
                if !cert.exists() {
                    return Err(anyhow::anyhow!("TLS certificate file not found: {}", cert.display()));
                }
                if !key.exists() {
                    return Err(anyhow::anyhow!("TLS key file not found: {}", key.display()));
                }
                tracing::info!("TLS enabled with certificate: {}", cert.display());
            }
            (Some(_), None) => {
                return Err(anyhow::anyhow!("--tls-key is required when --tls-cert is provided"));
            }
            (None, Some(_)) => {
                return Err(anyhow::anyhow!("--tls-cert is required when --tls-key is provided"));
            }
            (None, None) => unreachable!(),
        }
    } else {
        tracing::warn!("TLS disabled - using plaintext HTTP. Provide --tls-cert and --tls-key for HTTPS.");
    }

    // Initialize MCP server with Streamable HTTP transport
    let mcp_registry = std::sync::Arc::new(mcp::AgentRegistry::new());
    let peer_registry = mcp::server::new_peer_registry();

    // Initialize Tmux manager for agent message injection
    let tmux_manager = std::sync::Arc::new(ergatai_core::tmux::TmuxManager::new("ergatai-opencode"));
    tracing::info!("Tmux manager initialized (session: ergatai)");

    let mcp_cancellation_token = CancellationToken::new();
    let mcp_service = create_mcp_service(
        mcp_registry.clone(),
        peer_registry.clone(),
        tmux_manager.clone(),
        mcp_cancellation_token.clone(),
        args.sse_keep_alive,
    );
    tracing::info!(
        "MCP server initialized (protocol 2025-06-18, Streamable HTTP, SSE keep-alive: {}s)",
        args.sse_keep_alive
    );
    tracing::info!("Agent messaging: Tmux injection (preferred) + MCP notification (fallback)");

    // Start background peer reaper — detects abrupt disconnects (kill, network drop)
    // and cleans up stale agent registrations within 10 seconds.
    start_peer_reaper(
        mcp_registry.clone(),
        peer_registry.clone(),
        mcp_cancellation_token.clone(),
    );
    tracing::info!("Peer reaper started (10s interval, auto-cleans dead transports)");

    // Initialize NATS (embedded server + JetStream)
    match nats::init_nats().await {
        Ok(conn) => {
            tracing::info!("✅ NATS initialized successfully");
            // NATS connection is stored globally, can be retrieved via get_nats_connection()
            let _ = conn;
        }
        Err(e) => {
            tracing::error!("❌ Failed to initialize NATS: {}", e);
            return Err(anyhow::anyhow!("NATS initialization failed: {}", e));
        }
    }

    // Build application router
    let state = app_state_with_token(args.api_token.clone()).clone();
    // Rate limiting: 20 requests per second per Agent ID (prevents spam loops)
    let governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(20)
            .key_extractor(AgentKeyExtractor)
            .finish()
            .expect("Failed to build rate limiter config")
    );

    let app = Router::new()
        // REST API routes
        .route("/health", get(health_check))
        .route("/ready", get(readiness_check))
        .route("/metrics", get(metrics_endpoint))
        // TODO(middleware): Re-enable after HTTP client migration
        // .route("/api/v1/chat", post(create_chat))
        // .route("/api/v1/agents", get(list_agents))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/dag", post(submit_dag))
        .route("/api/v1/dag/status", get(dag_status))
        // MCP Streamable HTTP endpoint (POST/GET/DELETE /mcp)
        .nest_service("/mcp", mcp_service)
        // Auth middleware (exempts /health and /ready)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // Rate limiting layer (applies to all routes)
        .layer(GovernorLayer { config: governor_conf })
        .with_state(state);

    // Start server
    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --host '{}': {}", args.host, e))?;

    tracing::info!("API server listening on {}", addr);

    // Start server with or without TLS
    //
    // IMPORTANT: use `into_make_service_with_connect_info::<SocketAddr>()` so
    // the peer address is inserted into request extensions. Without this, the
    // `AgentKeyExtractor` IP fallback never fires and all non-MCP traffic
    // shares a single rate-limit bucket.
    let app_with_connect_info = app.into_make_service_with_connect_info::<SocketAddr>();

    if let (Some(cert_path), Some(key_path)) = (&args.tls_cert, &args.tls_key) {
        // TLS mode: use axum-server with rustls
        tracing::info!("Starting HTTPS server on {}", addr);
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load TLS certificate: {}", e))?;
        tracing::info!("TLS certificate loaded successfully");
        axum_server::bind_rustls(addr, tls_config)
            .serve(app_with_connect_info)
            .await?;
    } else {
        // Plain HTTP mode
        tracing::info!("Starting HTTP server on {}", addr);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app_with_connect_info).await?;
    }

    // Server has shut down — cleanly cancel the MCP service and NATS forwarder.
    // They may already be stopped by the signal handler, but this is idempotent.
    mcp_cancellation_token.cancel();

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    // Record health check request
    metrics::counter!("api_requests_total", "endpoint" => "health").increment(1);

    let mut checks = serde_json::Map::new();
    let mut all_healthy = true;

    // Check NATS connectivity
    let nats_ok = nats::is_nats_initialized().await;
    checks.insert("nats".to_string(), serde_json::Value::Bool(nats_ok));
    if !nats_ok {
        all_healthy = false;
    }

    // Check NATS connection is actually alive
    let nats_connected = if let Some(conn) = nats::get_nats_connection().await {
        conn.is_connected()
    } else {
        false
    };
    checks.insert("nats_connected".to_string(), serde_json::Value::Bool(nats_connected));
    if !nats_connected {
        all_healthy = false;
    }

    // Check NATS server port
    let nats_port = nats::get_nats_server_port().await;
    checks.insert("nats_port".to_string(), serde_json::Value::Number(
        nats_port.map(|p| p as u64).unwrap_or(0).into()
    ));

    let status = if all_healthy { "healthy" } else { "unhealthy" };
    let status_code = if all_healthy { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };

    (
        status_code,
        Json(serde_json::json!({
            "status": status,
            "checks": checks,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })),
    )
}

/// Kubernetes readiness probe — checks if the server is ready to accept traffic.
/// Returns 503 if any critical subsystem is down.
async fn readiness_check() -> impl IntoResponse {
    let nats_ready = nats::is_nats_initialized().await
        && nats::get_nats_connection().await.map(|c| c.is_connected()).unwrap_or(false);

    if nats_ready {
        (StatusCode::OK, Json(serde_json::json!({"ready": true})))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ready": false})))
    }
}

/// GET /metrics — Prometheus metrics endpoint.
/// Returns metrics in Prometheus text exposition format.
async fn metrics_endpoint() -> impl IntoResponse {
    match PROMETHEUS_HANDLE.get() {
        Some(handle) => {
            let output = handle.render();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                output,
            ).into_response()
        }
        None => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Metrics not initialized").into_response()
        }
    }
}

// ── Request / response types ──

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Reserved for future chat session API
struct CreateChatRequest {
    /// Agent name to start the session with (e.g. "claude", "codex", or a hosted agent name).
    agent: String,
    /// Optional working directory for the session. Falls back to the server default cwd.
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)] // Reserved for future chat session API
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
#[allow(dead_code)] // Reserved for future agent listing API
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
    active_agents: usize,
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
    // Health check, readiness probe, and metrics are always public
    let path = request.uri().path();
    if path == "/health" || path == "/ready" || path == "/metrics" {
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
#[allow(dead_code)] // Reserved for future chat session validation
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
/// Returns NATS connectivity status and connected agent count.
async fn get_status() -> impl IntoResponse {
    // Record status request metric
    metrics::counter!("api_requests_total", "endpoint" => "status").increment(1);

    let nats_initialized = nats::is_nats_initialized().await;
    let nats_port = nats::get_nats_server_port().await;

    let active_agents = ergatai_core::agent_registry::agent_registry().active_count().await;

    // Record active agents gauge
    metrics::gauge!("active_agents").set(active_agents as f64);

    Json(StatusResponse {
        nats_initialized,
        nats_port,
        active_agents,
    })
}

/// POST /api/v1/dag — submit a DAG workflow for execution.
///
/// Accepts a markdown-formatted DAG definition as the request body.
/// Parses it into a TaskGraph, creates a DagScheduler, and starts execution.
async fn submit_dag(body: String) -> impl IntoResponse {
    // Check if a DAG is already running
    if let Some(existing) = get_dag_scheduler() {
        if !existing.is_complete().await {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "A DAG is already running. Wait for completion or check status.",
                })),
            );
        }
    }

    // Parse markdown → TaskGraph
    let graph = match ergatai_core::orchestration::parse_dag_markdown(&body) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Failed to parse DAG definition: {}", e),
                })),
            );
        }
    };

    let state = APP_STATE.get().expect("AppState initialized");
    let project_root = PathBuf::from(&state.default_cwd);
    let scheduler = DagScheduler::new(project_root, graph);

    // Register globally + start event listener
    set_dag_scheduler(scheduler.clone());
    scheduler.clone().start_event_listener();

    // Submit the graph
    match scheduler.submit_graph().await {
        Ok(submitted) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "submitted",
                "submitted_nodes": submitted.len(),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to submit DAG: {}", e),
            })),
        ),
    }
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
