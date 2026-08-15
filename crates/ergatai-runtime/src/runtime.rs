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
    AGENT_RUNTIME.set(runtime.clone()).map_err(|_| {
        ErgataiError::internal("AgentRuntime already initialized".to_string())
    })?;
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
    pub async fn inject_message(
        &self,
        agent_id: &str,
        message: &str,
    ) -> ErgataiResult<()> {
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
                debug!(agent_id = agent_id, "Message delivered via backend injection");
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
    pub async fn set_task_id(
        &self,
        agent_id: &str,
        task_id: String,
    ) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;
        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.task_id = Some(task_id);
        Ok(())
    }

    /// Update agent state.
    pub async fn set_agent_state(
        &self,
        agent_id: &str,
        state: AgentState,
    ) -> ErgataiResult<()> {
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
                    warn!(agent_id = agent_id, signal = signal, "Agent killed by signal");
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
