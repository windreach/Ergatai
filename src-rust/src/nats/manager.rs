//! Global NATS manager for application-wide NATS access
//!
//! Provides lazy initialization of NATS server and connection.

use std::sync::OnceLock;
use tokio::sync::RwLock;
use tracing::info;

use crate::error::ErgataiResult;
use crate::nats::{NatsServer, NatsConnection};

/// Global NATS state
struct NatsState {
    server: Option<NatsServer>,
    connection: Option<NatsConnection>,
}

static NATS_STATE: OnceLock<RwLock<NatsState>> = OnceLock::new();

fn nats_state() -> &'static RwLock<NatsState> {
    NATS_STATE.get_or_init(|| RwLock::new(NatsState {
        server: None,
        connection: None,
    }))
}

/// Initialize NATS (start server + connect)
///
/// This is idempotent - calling multiple times is safe.
/// Returns the connection if successful.
pub async fn init_nats() -> ErgataiResult<NatsConnection> {
    let state = nats_state();
    let mut state = state.write().await;

    // Check if already initialized
    if let Some(conn) = &state.connection {
        if conn.is_connected() {
            info!("NATS already initialized");
            return Ok(conn.clone());
        }
    }

    // Start nats-server
    info!("Starting NATS server...");
    let server = NatsServer::start().await?;
    let port = server.port();
    info!(port = port, "NATS server started");

    // Connect to server
    info!("Connecting to NATS server...");
    let connection = NatsConnection::connect_to_server(&server).await?;
    info!("Connected to NATS server");

    state.server = Some(server);
    state.connection = Some(connection.clone());

    Ok(connection)
}

/// Get the current NATS connection (if initialized)
pub async fn get_nats_connection() -> Option<NatsConnection> {
    let state = nats_state();
    let state = state.read().await;
    state.connection.clone()
}

/// Check if NATS is initialized and connected
pub async fn is_nats_initialized() -> bool {
    let state = nats_state();
    let state = state.read().await;
    state.connection.as_ref().map(|c| c.is_connected()).unwrap_or(false)
}

/// Shutdown NATS (kill server + disconnect)
pub async fn shutdown_nats() {
    let state = nats_state();
    let mut state = state.write().await;

    info!("Shutting down NATS...");

    // Drop connection first
    state.connection = None;

    // Drop server (kills child process)
    state.server = None;

    info!("NATS shutdown complete");
}
