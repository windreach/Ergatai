//! HTTP ACP Client
//!
//! Provides HTTP-based ACP client connections to agents that are already running.
//! In middleware mode, agents expose ACP HTTP endpoints and Ergatai connects to them
//! via this module instead of spawning agent processes.

use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use agent_client_protocol::{Client, ConnectionTo, Agent};
use agent_client_protocol_http::HttpClient;
use anyhow::Result;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{error, info, warn};

use crate::manager::{SessionCommand, SessionKind};

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed, requests flow normally
    Closed,
    /// Circuit is open, requests are rejected
    Open,
    /// Circuit is half-open, testing if service recovered
    HalfOpen,
}

/// Circuit breaker for ACP HTTP connections.
///
/// Prevents cascading failures by stopping requests to unhealthy agents.
/// After N consecutive failures, the circuit opens for M seconds.
/// In half-open state, one successful request closes the circuit.
pub struct CircuitBreaker {
    /// Current state
    state: Arc<RwLock<CircuitState>>,
    /// Consecutive failure count
    failures: Arc<AtomicU32>,
    /// Failure threshold to open circuit
    failure_threshold: u32,
    /// Recovery timeout in seconds
    recovery_timeout_secs: u64,
    /// Timestamp when circuit was opened
    opened_at: Arc<AtomicU64>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default settings.
    ///
    /// Default: 5 failures threshold, 30 seconds recovery timeout.
    pub fn new() -> Self {
        Self::with_config(5, 30)
    }

    /// Create a circuit breaker with custom configuration.
    ///
    /// # Arguments
    /// * `failure_threshold` - Number of consecutive failures before opening
    /// * `recovery_timeout_secs` - Seconds to wait before trying half-open
    pub fn with_config(failure_threshold: u32, recovery_timeout_secs: u64) -> Self {
        Self {
            state: Arc::new(RwLock::new(CircuitState::Closed)),
            failures: Arc::new(AtomicU32::new(0)),
            failure_threshold,
            recovery_timeout_secs,
            opened_at: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Check if the circuit allows requests.
    ///
    /// Returns Ok(()) if requests are allowed, Err if circuit is open.
    pub async fn check(&self) -> Result<()> {
        let state = *self.state.read().await;
        match state {
            CircuitState::Closed => Ok(()),
            CircuitState::Open => {
                // Check if recovery timeout has elapsed
                let opened_at = self.opened_at.load(Ordering::SeqCst);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                if now - opened_at >= self.recovery_timeout_secs {
                    // Transition to half-open
                    let mut state = self.state.write().await;
                    *state = CircuitState::HalfOpen;
                    info!("Circuit breaker transitioning to half-open state");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "Circuit breaker is open, rejecting request. Retry after {} seconds",
                        self.recovery_timeout_secs - (now - opened_at)
                    ))
                }
            }
            CircuitState::HalfOpen => Ok(()), // Allow one test request
        }
    }

    /// Record a successful request.
    pub async fn record_success(&self) {
        self.failures.store(0, Ordering::SeqCst);
        let mut state = self.state.write().await;
        if *state == CircuitState::HalfOpen {
            *state = CircuitState::Closed;
            info!("Circuit breaker closed after successful request");
        }
    }

    /// Record a failed request.
    pub async fn record_failure(&self) {
        let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;

        if failures >= self.failure_threshold {
            let mut state = self.state.write().await;
            if *state != CircuitState::Open {
                *state = CircuitState::Open;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.opened_at.store(now, Ordering::SeqCst);
                warn!(
                    "Circuit breaker opened after {} consecutive failures. \
                     Will retry after {} seconds",
                    failures, self.recovery_timeout_secs
                );
            }
        }
    }

    /// Get current circuit state.
    pub async fn state(&self) -> CircuitState {
        *self.state.read().await
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

/// Connection to a remote agent via HTTP ACP.
///
/// Wraps the `HttpClient` from `agent-client-protocol-http` and provides
/// session management capabilities.
pub struct HttpAcpClient {
    /// The agent's ACP HTTP endpoint URL (e.g., "http://localhost:8080")
    endpoint: String,
    /// The underlying HTTP client
    http_client: HttpClient,
    /// Agent identifier
    agent_id: String,
}

impl HttpAcpClient {
    /// Create a new HTTP ACP client connecting to the given endpoint.
    ///
    /// # Arguments
    /// * `agent_id` - Identifier for the agent
    /// * `endpoint` - The agent's ACP HTTP endpoint URL (e.g., "http://localhost:8080")
    pub fn new(agent_id: &str, endpoint: &str) -> Result<Self> {
        let http_client = HttpClient::new(endpoint)
            .map_err(|e| anyhow::anyhow!("Failed to create HTTP client for {}: {}", endpoint, e))?;

        Ok(Self {
            endpoint: endpoint.to_string(),
            http_client,
            agent_id: agent_id.to_string(),
        })
    }

    /// Get the agent's endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Get the agent ID.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Establish a connection to the agent and create a session.
    ///
    /// Returns a `SessionHandle` that can be used to send commands to the agent.
    pub async fn connect(
        self,
        cwd: String,
        kind: SessionKind,
    ) -> Result<HttpSessionHandle> {
        let agent_id = self.agent_id.clone();
        let endpoint = self.endpoint.clone();
        let http_client = self.http_client;

        let (session_id_tx, session_id_rx) = oneshot::channel::<Result<String>>();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();

        // Clone values for use in the async block
        let endpoint_for_log = endpoint.clone();
        let cwd_for_task = cwd.clone();

        // Spawn the connection task (returns Result<()> to propagate errors)
        let connection_handle: tokio::task::JoinHandle<Result<()>> = tokio::spawn(async move {
            let _result = Client.v2()
                // Handle notifications from the agent (V2 protocol)
                .on_receive_notification(
                    async |notification: agent_client_protocol::schema::v2::UpdateSessionNotification,
                           _connection: ConnectionTo<Agent>| {
                        info!("Received V2 notification from agent: {:?}", notification.update);
                        // TODO: Forward to event bus
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                // Handle permission requests from the agent (V2 protocol)
                .on_receive_request(
                    async |request: agent_client_protocol::schema::v2::RequestPermissionRequest,
                           responder: agent_client_protocol::Responder<agent_client_protocol::schema::v2::RequestPermissionResponse>,
                           _connection: ConnectionTo<Agent>| {
                        // SECURITY: Log all permission requests for audit trail
                        tracing::warn!(
                            "🔒 PERMISSION REQUEST from agent: options_count={}",
                            request.options.len()
                        );
                        for (i, opt) in request.options.iter().enumerate() {
                            tracing::warn!(
                                "  Option {}: id='{}', name='{}'",
                                i, opt.option_id, opt.name
                            );
                        }

                        // Check permission policy
                        if crate::permission::is_auto_approve() {
                            tracing::warn!(
                                "⚠️  AUTO-APPROVING permission request (first option selected). \
                                 Auto-approve is enabled via --auto-approve flag."
                            );

                            let option_id = request.options.first().map(|o| o.option_id.clone());
                            if let Some(id) = option_id {
                                let _ = responder.respond(
                                    agent_client_protocol::schema::v2::RequestPermissionResponse::new(
                                        agent_client_protocol::schema::v2::RequestPermissionOutcome::Selected(
                                            agent_client_protocol::schema::v2::SelectedPermissionOutcome::new(id),
                                        ),
                                    ),
                                );
                                tracing::info!("✅ Permission granted (auto-approved)");
                            } else {
                                tracing::error!("❌ No permission options available, denying request");
                            }
                        } else {
                            // Deny permission request when auto-approve is disabled
                            tracing::warn!(
                                "🔒 DENYING permission request (auto-approve disabled). \
                                 Use --auto-approve to enable automatic approval."
                            );
                            // Deny: send Cancelled outcome
                            let _ = responder.respond(
                                agent_client_protocol::schema::v2::RequestPermissionResponse::new(
                                    agent_client_protocol::schema::v2::RequestPermissionOutcome::Cancelled,
                                ),
                            );
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // Connect to the agent via HTTP using V2 protocol
                .connect_with(http_client, async |cx| {
                    use agent_client_protocol::schema::v2::*;
                    use agent_client_protocol::schema::ProtocolVersion;

                    // Initialize the connection with V2 protocol
                    let _init_response = cx
                        .send_request(InitializeRequest::new(
                            ProtocolVersion::V2,
                            Implementation::new("ergatai", env!("CARGO_PKG_VERSION")),
                        ))
                        .block_task()
                        .await?;

                    info!("Connected to agent at {} with ACP V2 protocol", endpoint_for_log);

                    // Create a new session
                    let new_session = cx
                        .send_request(NewSessionRequest::new(std::path::PathBuf::from(&cwd_for_task)))
                        .block_task()
                        .await?;

                    let session_id = new_session.session_id.to_string();
                    info!("Created session {} with agent (V2)", session_id);

                    // Send session ID directly to caller
                    let _ = session_id_tx.send(Ok(session_id.clone()));

                    // Command loop - process commands from the session handle
                    let session_id_arc = SessionId::new(session_id);
                    let mut cmd_rx = cmd_rx;

                    loop {
                        match cmd_rx.recv().await {
                            Some(SessionCommand::SendPrompt { text, reply_tx }) => {
                                info!("Sending prompt to agent (V2)");
                                let result = cx
                                    .send_request(PromptRequest::new(
                                        session_id_arc.clone(),
                                        vec![ContentBlock::Text(TextContent::new(text))],
                                    ))
                                    .block_task()
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| anyhow::anyhow!("Prompt failed: {}", e));
                                let _ = reply_tx.send(result);
                            }
                            Some(SessionCommand::SetMode { mode_id, reply_tx }) => {
                                info!("Setting mode to {} (V2 - not yet implemented)", mode_id);
                                // TODO: Implement mode setting for V2
                                let _ = reply_tx.send(Ok(()));
                            }
                            Some(SessionCommand::Steer { text, reply_tx }) => {
                                info!("Steering agent with: {} (V2 - not yet implemented)", text);
                                // TODO: Implement steering for V2
                                let _ = reply_tx.send(Ok(()));
                            }
                            Some(SessionCommand::Close) | None => {
                                info!("Closing connection to agent (V2)");
                                break;
                            }
                            _ => {
                                // Other commands not yet implemented
                                warn!("Received unimplemented command");
                            }
                        }
                    }

                    Ok(())
                })
                .await
                .map_err(|e| {
                    error!("Connection to agent failed: {}", e);
                    // Note: session_id_tx was moved into the inner closure.
                    // If connection fails before session creation, the caller will
                    // get a "Session creation channel closed" error from session_id_rx.
                    anyhow::anyhow!("Connection failed: {}", e)
                })?;

            Ok(())
        });

        // Wait for session ID
        let session_id = session_id_rx
            .await
            .map_err(|_| anyhow::anyhow!("Session creation channel closed"))??;

        Ok(HttpSessionHandle {
            session_id,
            agent_id: agent_id.clone(),
            endpoint,
            cmd_tx,
            kind,
            connection_handle,
        })
    }
}

/// Handle to an active HTTP ACP session.
///
/// Provides methods to send commands to the connected agent.
pub struct HttpSessionHandle {
    /// The session ID
    pub session_id: String,
    /// The agent ID
    pub agent_id: String,
    /// The agent's endpoint
    pub endpoint: String,
    /// Command channel to the connection task
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    /// Session kind (Chat or Dag)
    pub kind: SessionKind,
    /// Handle to the connection task (propagates errors via Result<()>)
    connection_handle: tokio::task::JoinHandle<Result<()>>,
}

impl HttpSessionHandle {
    /// Send a prompt to the agent.
    pub async fn send_prompt(&self, text: String) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SendPrompt { text, reply_tx })
            .map_err(|_| anyhow::anyhow!("Command channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Reply channel closed"))?
    }

    /// Close the session.
    pub async fn close(self) -> Result<()> {
        let _ = self.cmd_tx.send(SessionCommand::Close);
        // Wait for the connection task to finish
        let _ = self.connection_handle.await;
        Ok(())
    }

    /// Get the command sender (for registering with SessionManager).
    pub fn cmd_tx(&self) -> mpsc::UnboundedSender<SessionCommand> {
        self.cmd_tx.clone()
    }
}

/// Connection manager for HTTP ACP clients.
///
/// Manages connections to multiple agents and provides session lifecycle management.
pub struct HttpConnectionManager {
    /// Active connections indexed by agent ID
    connections: Arc<RwLock<std::collections::HashMap<String, HttpSessionHandle>>>,
}

/// Global HTTP connection manager instance
static HTTP_CONNECTION_MANAGER: OnceLock<HttpConnectionManager> = OnceLock::new();

/// Get the global HTTP connection manager instance.
pub fn http_connection_manager() -> &'static HttpConnectionManager {
    HTTP_CONNECTION_MANAGER.get_or_init(HttpConnectionManager::new)
}

impl Default for HttpConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpConnectionManager {
    /// Create a new connection manager.
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Connect to an agent at the given endpoint.
    pub async fn connect(
        &self,
        agent_id: &str,
        endpoint: &str,
        cwd: String,
        kind: SessionKind,
    ) -> Result<String> {
        let client = HttpAcpClient::new(agent_id, endpoint)?;
        let handle = client.connect(cwd, kind).await?;
        let session_id = handle.session_id.clone();

        let mut connections = self.connections.write().await;
        connections.insert(agent_id.to_string(), handle);

        info!("Connected to agent {} at {}", agent_id, endpoint);
        Ok(session_id)
    }

    /// Disconnect from an agent.
    pub async fn disconnect(&self, agent_id: &str) -> Result<()> {
        let mut connections = self.connections.write().await;
        if let Some(handle) = connections.remove(agent_id) {
            handle.close().await?;
            info!("Disconnected from agent {}", agent_id);
        }
        Ok(())
    }

    /// Disconnect from all agents. Used during graceful shutdown.
    pub async fn disconnect_all(&self) {
        let mut connections = self.connections.write().await;
        let agent_ids: Vec<String> = connections.keys().cloned().collect();
        for agent_id in agent_ids {
            if let Some(handle) = connections.remove(&agent_id) {
                if let Err(e) = handle.close().await {
                    warn!("Error disconnecting from agent {}: {}", agent_id, e);
                } else {
                    info!("Disconnected from agent {}", agent_id);
                }
            }
        }
    }

    /// Send a prompt to a connected agent.
    pub async fn send_prompt(&self, agent_id: &str, text: String) -> Result<()> {
        let connections = self.connections.read().await;
        let handle = connections
            .get(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent {} not connected", agent_id))?;
        handle.send_prompt(text).await
    }

    /// List all connected agents.
    pub async fn list_connections(&self) -> Vec<(String, String)> {
        let connections = self.connections.read().await;
        connections
            .iter()
            .map(|(id, handle)| (id.clone(), handle.endpoint.clone()))
            .collect()
    }

    /// Check if an agent is connected.
    pub async fn is_connected(&self, agent_id: &str) -> bool {
        let connections = self.connections.read().await;
        connections.contains_key(agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_acp_client_creation() {
        let client = HttpAcpClient::new("test-agent", "http://localhost:8080");
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.agent_id(), "test-agent");
        assert_eq!(client.endpoint(), "http://localhost:8080");
    }

    #[tokio::test]
    async fn test_connection_manager_creation() {
        let manager = HttpConnectionManager::new();
        let connections = manager.list_connections().await;
        assert!(connections.is_empty());
    }

    #[tokio::test]
    async fn test_connection_manager_is_connected() {
        let manager = HttpConnectionManager::new();
        assert!(!manager.is_connected("nonexistent").await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state().await, CircuitState::Closed);
        assert!(cb.check().await.is_ok());
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let cb = CircuitBreaker::with_config(3, 30);

        // Record 3 failures
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_failure().await;

        // Circuit should be open
        assert_eq!(cb.state().await, CircuitState::Open);
        assert!(cb.check().await.is_err());
    }

    #[tokio::test]
    async fn test_circuit_breaker_success_resets_failures() {
        let cb = CircuitBreaker::with_config(3, 30);

        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_success().await; // Reset
        cb.record_failure().await;
        cb.record_failure().await;

        // Still closed (only 2 failures after reset)
        assert_eq!(cb.state().await, CircuitState::Closed);
    }
}
