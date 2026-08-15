//! NATS server process management
//!
//! Spawns nats-server binary as a child process and manages its lifecycle.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::sleep;
use tracing::{error, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

/// Default NATS server port
const DEFAULT_PORT: u16 = 4222;

/// Maximum port attempts before giving up
const MAX_PORT_ATTEMPTS: u16 = 10;

/// Time to wait for nats-server to start (milliseconds)
const STARTUP_WAIT_MS: u64 = 200;

/// Maximum number of retry attempts when port binding fails
const MAX_BIND_RETRIES: u32 = 3;

/// Delay between bind retries (milliseconds)
const BIND_RETRY_DELAY_MS: u64 = 50;

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
        let binary_path = Self::find_binary()?;

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
                    store_dir
                        .to_str()
                        .ok_or_else(|| ErgataiError::internal("Invalid NATS store directory path"))?,
                ])
                .spawn()
            {
                Ok(mut child) => {
                    sleep(Duration::from_millis(STARTUP_WAIT_MS)).await;

                    match child.try_wait() {
                        Ok(Some(status)) => {
                            warn!(port = port, status = %status, attempt = attempt + 1, "nats-server exited prematurely, retrying with different port");
                            last_error = Some(format!("nats-server exited with status: {}", status));
                            sleep(Duration::from_millis(BIND_RETRY_DELAY_MS)).await;
                            continue;
                        }
                        Ok(None) => {
                            info!(port = port, "nats-server started successfully");
                            return Ok(Self {
                                child: Some(child),
                                port,
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to check nats-server status");
                            return Ok(Self {
                                child: Some(child),
                                port,
                            });
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

    /// Locate the nats-server binary
    ///
    /// Searches in:
    /// 1. ERGATAI_NATS_BINARY environment variable
    /// 2. Electron resources/ directory (platform-specific)
    /// 3. System PATH (for development)
    fn find_binary() -> ErgataiResult<PathBuf> {
        // 1. Environment variable override
        if let Ok(path) = std::env::var("ERGATAI_NATS_BINARY") {
            let path = PathBuf::from(path);
            if path.exists() {
                return Ok(path);
            }
            warn!(path = %path.display(), "ERGATAI_NATS_BINARY points to non-existent file");
        }

        // 2. Electron resources directory
        // In production, nats-server is bundled in resources/nats-server-{platform}
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let platform = if cfg!(target_os = "macos") {
                    "darwin"
                } else if cfg!(target_os = "windows") {
                    "win32"
                } else {
                    "linux"
                };

                let binary_name = if cfg!(target_os = "windows") {
                    "nats-server.exe"
                } else {
                    "nats-server"
                };

                let resource_path = exe_dir
                    .join("resources")
                    .join(format!("nats-server-{}", platform))
                    .join(binary_name);

                if resource_path.exists() {
                    return Ok(resource_path);
                }
            }
        }

        // 3. System PATH (development fallback)
        if let Ok(output) = Command::new("which").arg("nats-server").output() {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = PathBuf::from(path_str.trim());
                if path.exists() {
                    warn!("Using nats-server from system PATH (not recommended for production)");
                    return Ok(path);
                }
            }
        }

        Err(ErgataiError::internal(
            "nats-server binary not found. Set ERGATAI_NATS_BINARY or install nats-server",
        ))
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
/// Uses a Mutex<Option<>> for lazy init + Box::leak to keep the server alive
/// until process exit (the child process is never dropped/killed).
static SHARED_TEST_SERVER: Mutex<Option<&'static NatsServer>> = Mutex::new(None);

/// Get a shared nats-server for testing.
///
/// Starts the server on first call with a fresh temp store directory
/// (avoids loading stale persistent data from production).
/// Returns the same instance on subsequent calls.
/// The server process lives until the test process exits.
///
/// Note: Holds std::sync::Mutex across async operation, which is acceptable here because:
/// 1. This is test-only code with one-time initialization
/// 2. Prevents race condition where multiple threads create zombie NATS processes
pub async fn shared_test_server() -> ErgataiResult<&'static NatsServer> {
    // Hold lock throughout initialization to prevent race condition
    let mut guard = SHARED_TEST_SERVER.lock().unwrap();

    // Check if already initialized
    if let Some(server) = *guard {
        return Ok(server);
    }

    // Start server with a unique temp store directory
    let store_dir = std::env::temp_dir()
        .join("ergatai-test-nats")
        .join(format!("pid-{}", std::process::id()));
    // Clean any stale data from previous runs with the same PID
    let _ = std::fs::remove_dir_all(&store_dir);

    let server = NatsServer::start_with_store_dir(store_dir).await?;
    let leaked: &'static NatsServer = Box::leak(Box::new(server));

    *guard = Some(leaked);
    Ok(leaked)
}

impl Drop for NatsServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            info!(port = self.port, "Killing nats-server");
            if let Err(e) = child.kill() {
                error!(error = %e, "Failed to kill nats-server");
            }
            // Reap the child process to prevent zombie
            if let Err(e) = child.wait() {
                error!(error = %e, "Failed to wait for nats-server");
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
        assert_eq!(MAX_PORT_ATTEMPTS, 10);
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
}
