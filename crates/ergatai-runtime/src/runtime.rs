//! AgentRuntime — high-level facade for agent management.
//!
//! Combines a backend, state tracking, and optional MCP integration into a
//! single API. This is what callers interact with instead of using the raw
//! backend trait directly.
//!
//! The runtime also provides a global singleton (`get_agent_runtime()`) to fix
//! the AgentLauncher lifetime bug — all components share the same runtime
//! instance instead of creating ephemeral managers.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use crate::backend::AgentRuntimeBackend;
use crate::mcp_integration::McpIntegration;
use crate::types::{AgentHandle, AgentInfo, AgentState, WaitResult, WorkspaceSpec};

// ── Global singleton ──

static AGENT_RUNTIME: OnceLock<Arc<AgentRuntime>> = OnceLock::new();

/// Get the global AgentRuntime singleton.
///
/// Initializes with `LocalPtyBackend` using the "ergatai" session prefix.
/// Call `init_agent_runtime()` instead if you need a custom backend.
pub fn get_agent_runtime() -> Arc<AgentRuntime> {
    AGENT_RUNTIME
        .get_or_init(|| {
            let backend = Arc::new(crate::backends::local_pty::LocalPtyBackend::new("ergatai"));
            Arc::new(AgentRuntime::new(backend))
        })
        .clone()
}

/// Initialize the global AgentRuntime with a custom backend.
///
/// Returns `Err` if already initialized. Call this from `main()` before
/// any other component accesses the runtime.
pub fn init_agent_runtime(
    backend: Arc<dyn AgentRuntimeBackend>,
) -> ErgataiResult<Arc<AgentRuntime>> {
    let runtime = Arc::new(AgentRuntime::new(backend));
    AGENT_RUNTIME
        .set(runtime.clone())
        .map_err(|_| ErgataiError::internal("AgentRuntime already initialized".to_string()))?;
    Ok(runtime)
}

// ── AgentRuntime ──

/// High-level facade for agent management.
///
/// Wraps a backend + agent registry + optional MCP integration.
pub struct AgentRuntime {
    backend: Arc<dyn AgentRuntimeBackend>,
    registry: Arc<RwLock<HashMap<String, AgentInfo>>>,
    mcp_integration: Arc<RwLock<Option<Arc<McpIntegration>>>>,
}

impl AgentRuntime {
    /// Create a new runtime with the given backend.
    pub fn new(backend: Arc<dyn AgentRuntimeBackend>) -> Self {
        Self {
            backend,
            registry: Arc::new(RwLock::new(HashMap::new())),
            mcp_integration: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the MCP integration for notification fallback.
    pub async fn set_mcp_integration(&self, mcp: Arc<McpIntegration>) {
        *self.mcp_integration.write().await = Some(mcp);
    }

    /// Get a reference to the underlying backend.
    pub fn backend(&self) -> &Arc<dyn AgentRuntimeBackend> {
        &self.backend
    }

    /// Initialize the backend.
    pub async fn initialize(&self) -> ErgataiResult<()> {
        self.backend.initialize().await
    }

    /// Launch an agent.
    ///
    /// Creates a workspace, starts the agent process, registers it, and
    /// spawns a background monitor.
    pub async fn launch_agent(
        &self,
        spec: WorkspaceSpec,
        command: &str,
        instruction: Option<&str>,
    ) -> ErgataiResult<String> {
        let workspace = self.backend.create_workspace(spec.clone()).await?;
        let handle = self
            .backend
            .start_agent(&workspace, command, instruction)
            .await?;

        let agent_id = handle.agent_id.clone();

        let info = AgentInfo {
            agent_id: agent_id.clone(),
            workspace_id: spec.id,
            handle: handle.clone(),
            state: AgentState::Running,
            task_id: None,
            mcp_agent_id: None,
            created_at: chrono::Utc::now(),
        };

        self.registry.write().await.insert(agent_id.clone(), info);
        self.spawn_monitor(agent_id.clone(), handle);

        info!(agent_id = agent_id, "Agent launched");
        Ok(agent_id)
    }

    /// Inject a message into a running agent.
    ///
    /// Tries backend injection first, falls back to MCP notification.
    pub async fn inject_message(&self, agent_id: &str, message: &str) -> ErgataiResult<()> {
        let info = {
            let registry = self.registry.read().await;
            registry
                .get(agent_id)
                .cloned()
                .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?
        };

        // Try backend injection
        match self.backend.inject_message(&info.handle, message).await {
            Ok(()) => {
                debug!(
                    agent_id = agent_id,
                    "Message delivered via backend injection"
                );
                return Ok(());
            }
            Err(e) => {
                debug!(
                    agent_id = agent_id,
                    error = %e,
                    "Backend injection failed, trying MCP fallback"
                );
            }
        }

        // Try MCP fallback
        let mcp = self.mcp_integration.read().await.clone();
        if let (Some(mcp), Some(mcp_id)) = (mcp, &info.mcp_agent_id) {
            return mcp.send_notification(mcp_id, message).await;
        }

        Err(ErgataiError::internal(format!(
            "All message delivery methods failed for agent {}",
            agent_id
        )))
    }

    /// Stop an agent.
    pub async fn stop_agent(&self, agent_id: &str) -> ErgataiResult<()> {
        let info = self
            .registry
            .write()
            .await
            .remove(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;

        if let Err(e) = self.backend.stop_agent(&info.handle).await {
            warn!(agent_id = agent_id, error = %e, "Failed to stop agent backend");
        }

        if let Err(e) = self.backend.cleanup_workspace(&info.handle.workspace).await {
            warn!(
                agent_id = agent_id,
                error = %e,
                "Failed to cleanup workspace"
            );
        }

        info!(agent_id = agent_id, "Agent stopped and cleaned up");
        Ok(())
    }

    /// List all registered agents.
    pub async fn list_agents(&self) -> Vec<AgentInfo> {
        self.registry.read().await.values().cloned().collect()
    }

    /// Get a specific agent by ID.
    pub async fn get_agent(&self, agent_id: &str) -> Option<AgentInfo> {
        self.registry.read().await.get(agent_id).cloned()
    }

    /// Set the MCP agent ID for a runtime agent (for notification routing).
    pub async fn set_mcp_agent_id(
        &self,
        agent_id: &str,
        mcp_agent_id: String,
    ) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;
        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.mcp_agent_id = Some(mcp_agent_id);
        Ok(())
    }

    /// Set the task ID for a runtime agent (for DAG tracking).
    pub async fn set_task_id(&self, agent_id: &str, task_id: String) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;
        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.task_id = Some(task_id);
        Ok(())
    }

    /// Update agent state.
    pub async fn set_agent_state(&self, agent_id: &str, state: AgentState) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;
        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.state = state;
        Ok(())
    }

    /// Capture agent output.
    pub async fn capture_output(&self, agent_id: &str) -> ErgataiResult<Option<String>> {
        let info = {
            let registry = self.registry.read().await;
            registry
                .get(agent_id)
                .cloned()
                .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?
        };

        self.backend.capture_output(&info.handle).await
    }

    /// Wait for agent to exit.
    pub async fn wait_for_exit(
        &self,
        agent_id: &str,
        timeout: Option<std::time::Duration>,
    ) -> ErgataiResult<WaitResult> {
        let info = {
            let registry = self.registry.read().await;
            registry
                .get(agent_id)
                .cloned()
                .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?
        };

        self.backend.wait_for_exit(&info.handle, timeout).await
    }

    /// Shutdown the runtime — stop all agents and cleanup all workspaces.
    pub async fn shutdown(&self) -> ErgataiResult<()> {
        let agents = self.list_agents().await;
        info!(count = agents.len(), "Shutting down agent runtime");

        for agent in &agents {
            if let Err(e) = self.stop_agent(&agent.agent_id).await {
                error!(agent_id = agent.agent_id, error = %e, "Failed to stop agent during shutdown");
            }
        }

        self.backend.shutdown().await?;
        info!("Agent runtime shutdown complete");
        Ok(())
    }

    /// Spawn a background monitor for an agent.
    fn spawn_monitor(&self, agent_id: String, handle: AgentHandle) {
        let backend = self.backend.clone();
        let registry = self.registry.clone();

        tokio::spawn(async move {
            match backend.wait_for_exit(&handle, None).await {
                Ok(crate::types::WaitResult::Exited { code }) => {
                    info!(agent_id = agent_id, code = code, "Agent exited");
                }
                Ok(crate::types::WaitResult::Signaled { signal }) => {
                    warn!(
                        agent_id = agent_id,
                        signal = signal,
                        "Agent killed by signal"
                    );
                }
                Ok(crate::types::WaitResult::Timeout) => {
                    warn!(agent_id = agent_id, "Agent monitor timed out (unexpected)");
                }
                Ok(crate::types::WaitResult::Error(e)) => {
                    error!(agent_id = agent_id, error = %e, "Agent monitor error");
                }
                Err(e) => {
                    error!(agent_id = agent_id, error = %e, "Agent wait failed");
                }
            }

            let mut reg = registry.write().await;
            if let Some(info) = reg.get_mut(&agent_id) {
                info.state = AgentState::Stopped;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentRuntimeBackend;
    use crate::types::{AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec};
    use ergatai_error::{ErgataiError, ErgataiResult};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// Mock backend that tracks calls and doesn't spawn processes.
    struct MockBackend {
        initialized: AtomicBool,
        workspace_count: AtomicUsize,
        inject_fail: bool,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                initialized: AtomicBool::new(false),
                workspace_count: AtomicUsize::new(0),
                inject_fail: false,
            }
        }

        fn with_inject_fail() -> Self {
            Self {
                initialized: AtomicBool::new(false),
                workspace_count: AtomicUsize::new(0),
                inject_fail: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentRuntimeBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_message_injection: !self.inject_fail,
                supports_output_capture: true,
                supports_resource_limits: false,
                supports_workspace_reuse: false,
                supports_network_isolation: false,
                max_concurrent_agents: None,
            }
        }
        async fn initialize(&self) -> ErgataiResult<()> {
            self.initialized.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle> {
            self.workspace_count.fetch_add(1, Ordering::SeqCst);
            Ok(WorkspaceHandle {
                id: spec.id.clone(),
                backend: "mock".to_string(),
                metadata: HashMap::new(),
            })
        }
        async fn start_agent(
            &self,
            handle: &WorkspaceHandle,
            _command: &str,
            _instruction: Option<&str>,
        ) -> ErgataiResult<AgentHandle> {
            Ok(AgentHandle {
                workspace: handle.clone(),
                agent_id: format!("agent-{}", handle.id),
                process_id: Some("12345".to_string()),
                metadata: HashMap::new(),
            })
        }
        async fn inject_message(&self, _handle: &AgentHandle, _message: &str) -> ErgataiResult<()> {
            if self.inject_fail {
                Err(ErgataiError::internal("inject failed".to_string()))
            } else {
                Ok(())
            }
        }
        async fn capture_output(&self, _handle: &AgentHandle) -> ErgataiResult<Option<String>> {
            Ok(Some("captured output".to_string()))
        }
        async fn is_alive(&self, _handle: &AgentHandle) -> ErgataiResult<bool> {
            Ok(false)
        }
        async fn stop_agent(&self, _handle: &AgentHandle) -> ErgataiResult<()> {
            Ok(())
        }
        async fn kill_agent(&self, _handle: &AgentHandle) -> ErgataiResult<()> {
            Ok(())
        }
        async fn wait_for_exit(
            &self,
            _handle: &AgentHandle,
            _timeout: Option<Duration>,
        ) -> ErgataiResult<WaitResult> {
            Ok(WaitResult::Exited { code: 0 })
        }
        async fn list_workspaces(&self) -> ErgataiResult<Vec<WorkspaceHandle>> {
            Ok(vec![])
        }
        async fn cleanup_workspace(&self, _handle: &WorkspaceHandle) -> ErgataiResult<()> {
            Ok(())
        }
        async fn shutdown(&self) -> ErgataiResult<()> {
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn make_spec(id: &str) -> WorkspaceSpec {
        WorkspaceSpec {
            id: id.to_string(),
            work_dir: PathBuf::from("/tmp"),
            env: HashMap::new(),
            resources: Default::default(),
            backend_config: serde_json::json!({}),
        }
    }

    fn make_runtime() -> AgentRuntime {
        AgentRuntime::new(Arc::new(MockBackend::new()))
    }

    #[test]
    fn test_runtime_new() {
        let runtime = make_runtime();
        assert_eq!(runtime.backend().name(), "mock");
    }

    #[tokio::test]
    async fn test_runtime_initialize() {
        let backend = Arc::new(MockBackend::new());
        let runtime = AgentRuntime::new(backend.clone());
        assert!(!backend.initialized.load(Ordering::SeqCst));
        runtime.initialize().await.unwrap();
        assert!(backend.initialized.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_launch_agent() {
        let runtime = make_runtime();
        let spec = make_spec("ws-1");
        let agent_id = runtime.launch_agent(spec, "cmd", None).await.unwrap();
        assert_eq!(agent_id, "agent-ws-1");
    }

    #[tokio::test]
    async fn test_launch_agent_registers() {
        let runtime = make_runtime();
        runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        let info = runtime.get_agent("agent-ws-1").await;
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.agent_id, "agent-ws-1");
        assert_eq!(info.workspace_id, "ws-1");
        assert_eq!(info.state, AgentState::Running);
    }

    #[tokio::test]
    async fn test_launch_multiple_agents() {
        let runtime = make_runtime();
        runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime
            .launch_agent(make_spec("ws-2"), "cmd", None)
            .await
            .unwrap();
        let agents = runtime.list_agents().await;
        assert_eq!(agents.len(), 2);
    }

    #[tokio::test]
    async fn test_get_agent_not_found() {
        let runtime = make_runtime();
        let result = runtime.get_agent("nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_list_agents_empty() {
        let runtime = make_runtime();
        let agents = runtime.list_agents().await;
        assert!(agents.is_empty());
    }

    #[tokio::test]
    async fn test_stop_agent() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime.stop_agent(&agent_id).await.unwrap();
        let info = runtime.get_agent(&agent_id).await;
        assert!(info.is_none());
    }

    #[tokio::test]
    async fn test_stop_agent_not_found() {
        let runtime = make_runtime();
        let result = runtime.stop_agent("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_inject_message_success() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime.inject_message(&agent_id, "hello").await.unwrap();
    }

    #[tokio::test]
    async fn test_inject_message_unknown_agent() {
        let runtime = make_runtime();
        let result = runtime.inject_message("nonexistent", "hello").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_inject_message_falls_back_to_mcp() {
        // Backend fails injection; MCP not configured → should fail
        let backend = Arc::new(MockBackend::with_inject_fail());
        let runtime = AgentRuntime::new(backend);
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        // No MCP integration set → error
        let result = runtime.inject_message(&agent_id, "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_mcp_agent_id() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime
            .set_mcp_agent_id(&agent_id, "mcp-1".to_string())
            .await
            .unwrap();
        let info = runtime.get_agent(&agent_id).await.unwrap();
        assert_eq!(info.mcp_agent_id, Some("mcp-1".to_string()));
    }

    #[tokio::test]
    async fn test_set_mcp_agent_id_not_found() {
        let runtime = make_runtime();
        let result = runtime
            .set_mcp_agent_id("nonexistent", "mcp-1".to_string())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_task_id() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime
            .set_task_id(&agent_id, "task-42".to_string())
            .await
            .unwrap();
        let info = runtime.get_agent(&agent_id).await.unwrap();
        assert_eq!(info.task_id, Some("task-42".to_string()));
    }

    #[tokio::test]
    async fn test_set_task_id_not_found() {
        let runtime = make_runtime();
        let result = runtime.set_task_id("nonexistent", "task-42".to_string()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_set_agent_state() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime
            .set_agent_state(&agent_id, AgentState::Stopping)
            .await
            .unwrap();
        let info = runtime.get_agent(&agent_id).await.unwrap();
        assert_eq!(info.state, AgentState::Stopping);
    }

    #[tokio::test]
    async fn test_set_agent_state_not_found() {
        let runtime = make_runtime();
        let result = runtime
            .set_agent_state("nonexistent", AgentState::Stopped)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_capture_output() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        let output = runtime.capture_output(&agent_id).await.unwrap();
        assert_eq!(output, Some("captured output".to_string()));
    }

    #[tokio::test]
    async fn test_capture_output_unknown_agent() {
        let runtime = make_runtime();
        let result = runtime.capture_output("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_wait_for_exit() {
        let runtime = make_runtime();
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        let result = runtime.wait_for_exit(&agent_id, None).await.unwrap();
        match result {
            WaitResult::Exited { code } => assert_eq!(code, 0),
            _ => panic!("Expected Exited"),
        }
    }

    #[tokio::test]
    async fn test_wait_for_exit_unknown_agent() {
        let runtime = make_runtime();
        let result = runtime.wait_for_exit("nonexistent", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown_stops_all() {
        let runtime = make_runtime();
        runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();
        runtime
            .launch_agent(make_spec("ws-2"), "cmd", None)
            .await
            .unwrap();
        runtime.shutdown().await.unwrap();
        let agents = runtime.list_agents().await;
        assert!(agents.is_empty());
    }

    #[tokio::test]
    async fn test_set_mcp_integration() {
        let runtime = make_runtime();
        let mcp = Arc::new(crate::mcp_integration::McpIntegration::new());
        runtime.set_mcp_integration(mcp).await;
        // Just verify it doesn't panic — mcp_integration is private
    }

    #[tokio::test]
    async fn test_inject_message_with_mcp_fallback() {
        // Backend fails injection; MCP integration set with a registered peer → success
        let backend = Arc::new(MockBackend::with_inject_fail());
        let runtime = AgentRuntime::new(backend);
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();

        let mcp = Arc::new(crate::mcp_integration::McpIntegration::new());
        mcp.register_peer("mcp-1".to_string(), |_: &str| async { Ok(()) })
            .await;
        runtime.set_mcp_integration(mcp).await;
        runtime
            .set_mcp_agent_id(&agent_id, "mcp-1".to_string())
            .await
            .unwrap();

        // Now inject should succeed via MCP fallback
        runtime.inject_message(&agent_id, "hello").await.unwrap();
    }

    #[tokio::test]
    async fn test_inject_message_mcp_set_but_no_agent_id() {
        // MCP integration set, but agent has no mcp_agent_id → fails
        let backend = Arc::new(MockBackend::with_inject_fail());
        let runtime = AgentRuntime::new(backend);
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();

        let mcp = Arc::new(crate::mcp_integration::McpIntegration::new());
        runtime.set_mcp_integration(mcp).await;
        // mcp_agent_id not set on agent
        let result = runtime.inject_message(&agent_id, "hello").await;
        assert!(result.is_err());
    }
}
