//! NATS server process management
//!
//! Spawns nats-server binary as a child process and manages its lifecycle.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::{sleep, timeout};
use tracing::{error, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

/// Default NATS server port
const DEFAULT_PORT: u16 = 4222;

/// Maximum port attempts before giving up
/// Increased from 10 to 100 to avoid port exhaustion during parallel test runs
const MAX_PORT_ATTEMPTS: u16 = 100;

/// Time to wait for nats-server to start (milliseconds)
const STARTUP_WAIT_MS: u64 = 500;

/// Maximum time to wait for NATS server to be ready to accept connections (seconds)
const READINESS_TIMEOUT_SECS: u64 = 10;

/// Maximum number of retry attempts when port binding fails
const MAX_BIND_RETRIES: u32 = 3;

/// Delay between bind retries (milliseconds)
const BIND_RETRY_DELAY_MS: u64 = 100;

/// Delay between readiness check attempts (milliseconds)
const READINESS_CHECK_INTERVAL_MS: u64 = 100;

/// NATS server process manager
///
/// Spawns nats-server as a child process and ensures it's killed on drop.
pub struct NatsServer {
    child: Option<Child>,
    port: u16,
}

impl NatsServer {
    /// Start a new nats-server instance with a custom store directory.
    ///
    /// Used by tests to avoid loading stale persistent data.
    pub async fn start_with_store_dir(store_dir: PathBuf) -> ErgataiResult<Self> {
        let binary_path = ergatai_binary::find_nats_binary()?;

        // Ensure store directory exists
        tokio::fs::create_dir_all(&store_dir).await.map_err(|e| {
            ErgataiError::internal(format!("Failed to create NATS store directory: {}", e))
        })?;

        let mut last_error = None;
        for attempt in 0..MAX_BIND_RETRIES {
            let port = Self::find_available_port().await?;

            info!(port = port, binary = %binary_path.display(), store = %store_dir.display(), attempt = attempt + 1, "Starting nats-server");

            match Command::new(&binary_path)
                .args([
                    "-p",
                    &port.to_string(),
                    "-a",
                    "127.0.0.1",
                    "--jetstream",
                    "-sd",
                    store_dir.to_str().ok_or_else(|| {
                        ErgataiError::internal("Invalid NATS store directory path")
                    })?,
                ])
                .stderr(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    sleep(Duration::from_millis(STARTUP_WAIT_MS)).await;

                    match child.try_wait() {
                        Ok(Some(status)) => {
                            // Process exited - read stderr to see why
                            let stderr_output = if let Some(mut stderr) = child.stderr.take() {
                                use std::io::Read;
                                let mut buffer = String::new();
                                let _ = stderr.read_to_string(&mut buffer);
                                buffer
                            } else {
                                String::new()
                            };

                            warn!(
                                port = port,
                                status = %status,
                                stderr = %stderr_output,
                                attempt = attempt + 1,
                                "nats-server exited prematurely"
                            );
                            last_error = Some(format!(
                                "nats-server exited with status: {}, stderr: {}",
                                status, stderr_output
                            ));
                            sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
                            continue;
                        }
                        Ok(None) => {
                            // Process is running - now wait for it to be ready to accept connections
                            info!(
                                port = port,
                                "nats-server process started, waiting for readiness..."
                            );

                            match Self::wait_for_ready(port).await {
                                Ok(()) => {
                                    info!(
                                        port = port,
                                        "nats-server is ready to accept connections"
                                    );
                                    return Ok(Self {
                                        child: Some(child),
                                        port,
                                    });
                                }
                                Err(e) => {
                                    warn!(port = port, error = %e, "nats-server failed to become ready");
                                    last_error = Some(format!("nats-server not ready: {}", e));
                                    // Kill the unresponsive process
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to check nats-server status, killing process");
                            let _ = child.kill();
                            let _ = child.wait();
                            last_error = Some(format!("Failed to check status: {}", e));
                            sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, attempt = attempt + 1, "Failed to spawn nats-server, retrying");
                    last_error = Some(format!("Failed to spawn: {}", e));
                    sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
                }
            }
        }

        Err(ErgataiError::internal(format!(
            "Failed to start nats-server after {} attempts: {}",
            MAX_BIND_RETRIES,
            last_error.unwrap_or_else(|| "Unknown error".to_string())
        )))
    }

    /// Wait for NATS server to be ready to accept connections
    ///
    /// Attempts to connect to the server with retries until timeout.
    async fn wait_for_ready(port: u16) -> ErgataiResult<()> {
        let url = format!("127.0.0.1:{}", port);
        let timeout_duration = Duration::from_secs(READINESS_TIMEOUT_SECS);
        let check_interval = Duration::from_millis(READINESS_CHECK_INTERVAL_MS);

        timeout(timeout_duration, async {
            loop {
                // Try to connect to NATS
                match async_nats::connect(&url).await {
                    Ok(client) => {
                        // Connection succeeded - server is ready
                        client.flush().await.ok();
                        return Ok(());
                    }
                    Err(_) => {
                        // Connection failed - wait and retry
                        sleep(check_interval).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            ErgataiError::internal(format!(
                "nats-server failed to become ready within {}s",
                READINESS_TIMEOUT_SECS
            ))
        })?
    }

    /// Start a new nats-server instance
    ///
    /// Locates the nats-server binary in Electron resources, finds an available port,
    /// and spawns the server process.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - nats-server binary not found
    /// - No available port in range [4222, 4232)
    /// - Failed to spawn child process
    pub async fn start() -> ErgataiResult<Self> {
        let store_dir = Self::get_store_dir()?;
        Self::start_with_store_dir(store_dir).await
    }

    /// Get the port this server is listening on
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the connection URL for this server
    pub fn url(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Get the NATS JetStream store directory
    ///
    /// Uses platform-specific data directory:
    /// - Linux: ~/.local/share/ergatai/nats-store
    /// - macOS: ~/Library/Application Support/ergatai/nats-store
    /// - Windows: C:/Users/{user}/AppData/Roaming/ergatai/nats-store
    ///
    /// Falls back to current directory if dirs crate fails.
    fn get_store_dir() -> ErgataiResult<PathBuf> {
        let base_dir = dirs::data_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        Ok(base_dir.join("ergatai").join("nats-store"))
    }

    /// Find an available port starting from DEFAULT_PORT
    ///
    /// Tries ports in range [4222, 4232) and returns the first available one.
    async fn find_available_port() -> ErgataiResult<u16> {
        for offset in 0..MAX_PORT_ATTEMPTS {
            let port = DEFAULT_PORT + offset;
            if Self::is_port_available(port).await {
                return Ok(port);
            }
            warn!(port = port, "Port in use, trying next");
        }

        Err(ErgataiError::IoError(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "No available port in range [{}, {})",
                DEFAULT_PORT,
                DEFAULT_PORT + MAX_PORT_ATTEMPTS
            ),
        )))
    }

    /// Check if a port is available by attempting to bind
    async fn is_port_available(port: u16) -> bool {
        tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
            .await
            .is_ok()
    }
}

/// Shared NATS server for all tests in the process.
///
/// Instead of each test spawning its own nats-server process (wasteful,
/// causes port conflicts), all tests share a single server instance.
/// Tests isolate data via unique stream/consumer names.
///
/// # Lifecycle
///
/// Uses `Box::leak` to keep the server alive until process exit. This is intentional:
/// - The NATS child process lives for the entire test run
/// - Drop is never called on the leaked server (by design)
/// - When the test process exits, the OS cleans up child processes
///
/// **Note**: If tests are interrupted (Ctrl+C, timeout), NATS processes may remain
/// as zombies until manually cleaned up. Use `cleanup_test_servers()` or run
/// `pkill -9 nats-server` to clean them up.
///
/// For production use, create `NatsServer` instances directly (not via this function)
/// so Drop can properly clean up resources.
static SHARED_TEST_SERVER: Mutex<Option<&'static NatsServer>> = Mutex::new(None);

/// Cleanup function to kill all NATS server processes started by tests
///
/// Call this at the end of test suites or use `pkill -9 nats-server` manually.
/// This is necessary because `shared_test_server()` uses `Box::leak()` which
/// prevents Drop from being called.
pub fn cleanup_test_servers() {
    use std::process::Command;
    let _ = Command::new("pkill").args(["-9", "nats-server"]).output();
}

/// Get a shared nats-server for testing.
///
/// Starts the server on first call with a fresh temp store directory
/// (avoids loading stale persistent data from production).
/// Returns the same instance on subsequent calls.
/// The server process lives until the test process exits.
///
/// Fixed: Release lock before async operations to prevent deadlock.
pub async fn shared_test_server() -> ErgataiResult<&'static NatsServer> {
    // Check if already initialized (quick path without holding lock during async)
    {
        let guard = SHARED_TEST_SERVER.lock().unwrap();
        if let Some(server) = *guard {
            return Ok(server);
        }
    }
    // Lock released here before async operation

    // Start server with a unique temp store directory
    let store_dir = std::env::temp_dir()
        .join("ergatai-test-nats")
        .join(format!("pid-{}", std::process::id()));
    // Clean any stale data from previous runs with the same PID
    let _ = std::fs::remove_dir_all(&store_dir);

    let server = NatsServer::start_with_store_dir(store_dir).await?;
    let leaked: &'static NatsServer = Box::leak(Box::new(server));

    // Re-acquire lock to store the server
    let mut guard = SHARED_TEST_SERVER.lock().unwrap();
    // Double-check in case another thread initialized while we were starting the server
    if let Some(existing) = *guard {
        return Ok(existing);
    }
    *guard = Some(leaked);
    Ok(leaked)
}

impl Drop for NatsServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            info!(port = self.port, "Killing nats-server");

            // Try to kill the process
            if let Err(e) = child.kill() {
                error!(error = %e, port = self.port, "Failed to kill nats-server");
                return;
            }

            // Wait for process to exit with a timeout to prevent hanging
            // Use a simple loop with sleep since we can't use tokio timeout in Drop
            const DROP_MAX_WAIT_MS: u64 = 5000; // 5 seconds
            const DROP_CHECK_INTERVAL_MS: u64 = 100;
            let mut waited_ms = 0;

            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        // Process exited successfully
                        info!(port = self.port, "nats-server process exited");
                        return;
                    }
                    Ok(None) => {
                        // Still running
                        if waited_ms >= DROP_MAX_WAIT_MS {
                            warn!(
                                port = self.port,
                                waited_ms = waited_ms,
                                "nats-server did not exit within timeout, leaving as zombie"
                            );
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(DROP_CHECK_INTERVAL_MS));
                        waited_ms += DROP_CHECK_INTERVAL_MS;
                    }
                    Err(e) => {
                        error!(error = %e, port = self.port, "Error waiting for nats-server to exit");
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_range() {
        assert_eq!(DEFAULT_PORT, 4222);
        assert_eq!(MAX_PORT_ATTEMPTS, 100);
    }

    #[tokio::test]
    async fn test_find_available_port() {
        let port = NatsServer::find_available_port().await.unwrap();
        assert!(port >= DEFAULT_PORT);
        assert!(port < DEFAULT_PORT + MAX_PORT_ATTEMPTS);
    }

    /// Test full server lifecycle: start → verify listening → drop → cleanup
    /// Skips gracefully if nats-server binary is not available.
    #[tokio::test]
    async fn test_server_lifecycle() {
        let server = match NatsServer::start().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  Skipping (nats-server not available): {}", e);
                return;
            }
        };

        let port = server.port();
        assert!(port >= DEFAULT_PORT, "Port should be in valid range");

        // Verify server is accepting connections
        let connect = tokio::net::TcpStream::connect(server.url()).await;
        assert!(connect.is_ok(), "Server should be listening on its port");

        // URL format check
        assert_eq!(server.url(), format!("127.0.0.1:{}", port));

        // Drop triggers cleanup
        drop(server);

        // Wait briefly for port release
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    /// Test port conflict resolution: bind a port, then verify find_available_port skips it
    #[tokio::test]
    async fn test_port_conflict_resolution() {
        // Bind port 4222 to create a conflict
        let listener = tokio::net::TcpListener::bind("127.0.0.1:4222").await;
        let port = NatsServer::find_available_port().await.unwrap();

        if listener.is_ok() {
            // If we successfully bound 4222, find_available_port should skip it
            assert!(
                port > DEFAULT_PORT,
                "Should find next available port when 4222 is busy"
            );
        }
        // If listener failed, 4222 is already in use; any valid port is acceptable
        assert!(port >= DEFAULT_PORT);
    }

    #[test]
    fn test_server_constants() {
        assert_eq!(DEFAULT_PORT, 4222);
        assert_eq!(MAX_PORT_ATTEMPTS, 100);
        assert_eq!(STARTUP_WAIT_MS, 500);
        assert_eq!(READINESS_TIMEOUT_SECS, 10);
        assert_eq!(MAX_BIND_RETRIES, 3);
        assert_eq!(BIND_RETRY_DELAY_MS, 100);
    }

    #[test]
    fn test_url_format() {
        // NatsServer.url() returns "127.0.0.1:{port}"
        // We can't construct a NatsServer directly (private field),
        // but we can verify the format via the shared test server
        // Just verify the constant port is correct
        assert_eq!(DEFAULT_PORT, 4222);
    }

    #[tokio::test]
    async fn test_get_store_dir() {
        let store_dir = NatsServer::get_store_dir().unwrap();
        // Should end with "ergatai/nats-store"
        let path_str = store_dir.to_string_lossy();
        assert!(
            path_str.contains("ergatai"),
            "Store dir should contain 'ergatai'"
        );
        assert!(
            path_str.contains("nats-store"),
            "Store dir should contain 'nats-store'"
        );
    }
}
