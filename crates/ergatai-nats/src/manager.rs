//! Global NATS manager for application-wide NATS access
//!
//! Provides lazy initialization of NATS server and connection.

use std::sync::OnceLock;
use tokio::sync::RwLock;
use tracing::info;

use crate::agent_message_stream::all_agent_message_stream_configs;
use crate::dag_event_stream::all_dag_event_stream_configs;
use crate::file_access_streams::all_file_access_stream_configs;
use crate::{NatsConnection, NatsServer};
use ergatai_error::ErgataiResult;

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

    // Fast path: check if already initialized (read lock only)
    {
        let state = state.read().await;
        if let Some(conn) = &state.connection {
            if conn.is_connected() {
                info!("NATS already initialized");
                return Ok(conn.clone());
            }
        }
    } // read lock released

    // Slow path: perform expensive I/O outside of any lock
    info!("Starting NATS server...");
    let server = NatsServer::start().await?;
    let port = server.port();
    info!(port = port, "NATS server started");

    info!("Connecting to NATS server...");
    let connection = NatsConnection::connect_to_server(&server).await?;
    info!("Connected to NATS server");

    info!("Initializing JetStream streams...");
    init_jetstream_streams(&connection).await?;

    // Acquire write lock and double-check (another task may have initialized concurrently)
    let mut state = state.write().await;
    if let Some(existing_conn) = &state.connection {
        if existing_conn.is_connected() {
            info!("NATS initialized by another task while we were connecting");
            // Our newly-started server is redundant — drop triggers NatsServer::Drop
            // which kills the child process cleanly.
            return Ok(existing_conn.clone());
        }
    }

    // Clean up stale state: if a prior connection existed but is now disconnected,
    // explicitly drop it (and its server) before replacing. This prevents zombie
    // NATS processes from accumulating across re-init cycles.
    if state.server.is_some() || state.connection.is_some() {
        info!("Replacing stale/disconnected NATS state");
        state.connection = None; // drop old connection first (uses server)
        state.server = None; // drop old server (kills child process)
    }

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
    use ergatai_error::ErgataiError;

    let jetstream = async_nats::jetstream::new(connection.client().clone());

    let configs = [
        all_file_access_stream_configs(),
        all_agent_message_stream_configs(),
        all_dag_event_stream_configs(),
    ]
    .concat();
    info!("Creating {} JetStream streams", configs.len());

    for config in configs {
        let stream_name = config.name.clone();
        let expected_retention = config.retention;
        let expected_max_age = config.max_age;

        // Use get_or_create_stream for atomic check-and-create.
        // The previous get_stream + create_stream pattern had a TOCTOU race:
        // two concurrent init calls could both fail get_stream and both attempt
        // create_stream, causing one to fail with "stream already exists".
        match jetstream.get_or_create_stream(config).await {
            Ok(mut stream) => {
                // Verify that an existing stream's config matches what we expect.
                // get_or_create_stream does NOT update an existing stream's config,
                // so if retention/max_age drifted between versions, we need to warn.
                if let Ok(info) = stream.info().await {
                    let actual_retention = info.config.retention;
                    let actual_max_age = info.config.max_age;
                    if actual_retention != expected_retention || actual_max_age != expected_max_age
                    {
                        tracing::warn!(
                            stream = %stream_name,
                            ?expected_retention,
                            ?actual_retention,
                            expected_max_age_secs = expected_max_age.as_secs(),
                            actual_max_age_secs = actual_max_age.as_secs(),
                            "Stream config mismatch — existing stream has different settings. \
                             Delete and recreate the stream to apply new config."
                        );
                    }
                }
                info!("JetStream stream '{}' ready", stream_name);
            }
            Err(e) => {
                return Err(ErgataiError::NatsError(format!(
                    "Failed to initialize JetStream stream '{}': {}",
                    stream_name, e
                )));
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
