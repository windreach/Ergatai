//! Global NATS manager for application-wide NATS access
//!
//! Provides lazy initialization of NATS server and connection.

use std::sync::OnceLock;
use tokio::sync::RwLock;
use tracing::info;

use crate::error::ErgataiResult;
use crate::nats::file_access_streams::all_file_access_stream_configs;
use crate::nats::{NatsConnection, NatsServer};

/// Global NATS state
struct NatsState {
    server: Option<NatsServer>,
    connection: Option<NatsConnection>,
}

static NATS_STATE: OnceLock<RwLock<NatsState>> = OnceLock::new();

fn nats_state() -> &'static RwLock<NatsState> {
    NATS_STATE.get_or_init(|| {
        RwLock::new(NatsState {
            server: None,
            connection: None,
        })
    })
}

/// Initialize NATS (start server + connect + create JetStream streams)
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

    // Initialize JetStream streams (fail-fast: a missing stream breaks persistence
    // guarantees for file access control and task queues, so surface the error).
    info!("Initializing JetStream streams...");
    init_jetstream_streams(&connection).await?;

    state.server = Some(server);
    state.connection = Some(connection.clone());

    Ok(connection)
}

/// Initialize all JetStream streams for file access control
///
/// Creates streams defined in `all_file_access_stream_configs()` if they don't exist.
/// This ensures message persistence and reliability for critical file access events.
///
/// # Errors
///
/// Returns an error if any required stream cannot be obtained or created. The caller
/// decides whether to fail-fast or degrade — errors are never silently swallowed here.
async fn init_jetstream_streams(connection: &NatsConnection) -> ErgataiResult<()> {
    use crate::error::ErgataiError;

    let jetstream = async_nats::jetstream::new(connection.client().clone());

    let configs = all_file_access_stream_configs();
    info!("Creating {} JetStream streams", configs.len());

    for config in configs {
        let stream_name = config.name.clone();

        // Try to get existing stream, create if not exists
        match jetstream.get_stream(&stream_name).await {
            Ok(_) => {
                info!("JetStream stream '{}' already exists", stream_name);
            }
            Err(get_err) => {
                // Stream doesn't exist (or lookup failed) — attempt to create it.
                match jetstream.create_stream(config).await {
                    Ok(_) => {
                        info!("Created JetStream stream '{}'", stream_name);
                    }
                    Err(create_err) => {
                        // Propagate the error so the caller can decide how to react.
                        // We surface both the initial lookup error and the create error
                        // to aid diagnosis.
                        return Err(ErgataiError::NatsError(format!(
                            "Failed to initialize JetStream stream '{}': lookup failed: {}; create failed: {}",
                            stream_name, get_err, create_err
                        )));
                    }
                }
            }
        }
    }

    Ok(())
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
    state
        .connection
        .as_ref()
        .map(|c| c.is_connected())
        .unwrap_or(false)
}

/// Get the port the NATS server is listening on
///
/// Returns None if NATS is not initialized or the server has been shut down.
pub async fn get_nats_server_port() -> Option<u16> {
    let state = nats_state();
    let state = state.read().await;
    state.server.as_ref().map(|s| s.port())
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
