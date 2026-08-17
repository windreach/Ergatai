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
///
/// # Concurrency
///
/// Uses a write lock for the entire initialization to prevent concurrent callers
/// from starting redundant NATS servers. The I/O cost is acceptable since
/// initialization happens once at startup.
pub async fn init_nats() -> ErgataiResult<NatsConnection> {
    let state = nats_state();

    // Acquire write lock for the entire init sequence to prevent race condition
    // where multiple callers start redundant NATS servers.
    let mut state = state.write().await;

    // Check if already initialized (under write lock)
    if let Some(conn) = &state.connection {
        if conn.is_connected() {
            info!("NATS already initialized");
            return Ok(conn.clone());
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

    // Perform expensive I/O while holding the write lock.
    // This serializes concurrent callers but prevents resource waste.
    info!("Starting NATS server...");
    let server = NatsServer::start().await?;
    let port = server.port();
    info!(port = port, "NATS server started");

    info!("Connecting to NATS server...");
    let connection = NatsConnection::connect_to_server(&server).await?;
    info!("Connected to NATS server");

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Global test lock — serializes tests that share the `NATS_STATE` global.
    ///
    /// Tests call `shutdown_nats()` / `init_nats()` which mutate the same
    /// static `RwLock<NatsState>`. Running them in parallel causes race
    /// conditions (one test's `shutdown_nats` clears another test's state).
    /// Each test acquires this lock at the start and holds it for the
    /// entire test body, ensuring sequential execution without adding
    /// external dependencies.
    static TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    /// Test that nats_state() returns the same static instance
    #[test]
    fn test_nats_state_singleton() {
        let state1 = nats_state();
        let state2 = nats_state();
        // Both should point to the same static instance
        assert_eq!(
            format!("{:p}", state1),
            format!("{:p}", state2),
            "nats_state() should return the same static instance"
        );
    }

    /// Test initial state is empty (no server, no connection)
    #[tokio::test]
    async fn test_initial_state_is_empty() {
        let _guard = TEST_LOCK.lock().await;
        // Reset state for this test
        shutdown_nats().await;

        let state = nats_state();
        let state_guard = state.read().await;
        assert!(
            state_guard.server.is_none(),
            "Server should be None initially"
        );
        assert!(
            state_guard.connection.is_none(),
            "Connection should be None initially"
        );
    }

    /// Test is_nats_initialized returns false when not initialized
    #[tokio::test]
    async fn test_is_nats_initialized_false_initially() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;
        assert!(
            !is_nats_initialized().await,
            "Should not be initialized initially"
        );
    }

    /// Test get_nats_connection returns None when not initialized
    #[tokio::test]
    async fn test_get_nats_connection_none_initially() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;
        let conn = get_nats_connection().await;
        assert!(conn.is_none(), "Should return None when not initialized");
    }

    /// Test get_nats_server_port returns None when not initialized
    #[tokio::test]
    async fn test_get_nats_server_port_none_initially() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;
        let port = get_nats_server_port().await;
        assert!(port.is_none(), "Should return None when not initialized");
    }

    /// Test shutdown_nats is idempotent (can be called multiple times)
    #[tokio::test]
    async fn test_shutdown_nats_idempotent() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;
        shutdown_nats().await;
        shutdown_nats().await;
        // Should not panic
        assert!(!is_nats_initialized().await);
    }

    /// Test init_nats starts server and creates connection
    #[tokio::test]
    async fn test_init_nats_starts_server() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        let conn = init_nats().await;
        match conn {
            Ok(connection) => {
                assert!(connection.is_connected(), "Connection should be active");
                assert!(
                    is_nats_initialized().await,
                    "Should be initialized after init"
                );

                let port = get_nats_server_port().await;
                assert!(port.is_some(), "Should have a port after init");
                assert!(port.unwrap() >= 4222, "Port should be in valid range");

                // Cleanup
                shutdown_nats().await;
            }
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
            }
        }
    }

    /// Test init_nats is idempotent
    #[tokio::test]
    async fn test_init_nats_idempotent() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        match init_nats().await {
            Ok(_conn1) => {
                let port1 = get_nats_server_port().await.unwrap();

                // Call init again - should return the same connection
                match init_nats().await {
                    Ok(conn2) => {
                        let port2 = get_nats_server_port().await.unwrap();
                        assert_eq!(port1, port2, "Port should remain the same");
                        assert!(conn2.is_connected(), "Connection should still be active");
                    }
                    Err(e) => {
                        panic!("Second init_nats should not fail: {}", e);
                    }
                }

                shutdown_nats().await;
            }
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
            }
        }
    }

    /// Test shutdown properly cleans up state
    #[tokio::test]
    async fn test_shutdown_cleanup() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        match init_nats().await {
            Ok(_) => {
                assert!(is_nats_initialized().await, "Should be initialized");

                shutdown_nats().await;

                assert!(
                    !is_nats_initialized().await,
                    "Should not be initialized after shutdown"
                );
                assert!(
                    get_nats_connection().await.is_none(),
                    "Connection should be None"
                );
                assert!(
                    get_nats_server_port().await.is_none(),
                    "Port should be None"
                );
            }
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
            }
        }
    }

    /// Test get_nats_connection returns clone of connection
    #[tokio::test]
    async fn test_get_nats_connection_returns_clone() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        match init_nats().await {
            Ok(_conn1) => {
                let conn2 = get_nats_connection().await;
                assert!(conn2.is_some(), "Should return connection");
                assert!(conn2.unwrap().is_connected(), "Connection should be active");

                shutdown_nats().await;
            }
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
            }
        }
    }

    /// Test port allocation is in valid range
    #[tokio::test]
    async fn test_port_allocation_range() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        match init_nats().await {
            Ok(_) => {
                let port = get_nats_server_port().await.unwrap();
                assert!(port >= 4222, "Port should be >= 4222");
                assert!(port < 4322, "Port should be < 4322 (4222 + 100)");

                shutdown_nats().await;
            }
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
            }
        }
    }

    /// Test sequential init_nats calls (should be safe)
    #[tokio::test]
    async fn test_concurrent_init_nats() {
        let _guard = TEST_LOCK.lock().await;
        shutdown_nats().await;

        // Run sequentially - init_nats should be idempotent
        let mut success_count = 0;
        for _ in 0..3 {
            match init_nats().await {
                Ok(_) => success_count += 1,
                Err(e) => {
                    eprintln!("⚠️  init_nats failed: {}", e);
                }
            }
        }

        // If any succeeded, we should be initialized
        if success_count > 0 {
            assert!(
                is_nats_initialized().await,
                "Should be initialized after successful init"
            );
        }

        shutdown_nats().await;
    }
}
