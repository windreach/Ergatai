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

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use crate::backend::AgentRuntimeBackend;
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
/// Wraps a backend + agent registry.
pub struct AgentRuntime {
    backend: Arc<dyn AgentRuntimeBackend>,
    registry: Arc<RwLock<HashMap<String, AgentInfo>>>,
    /// Reverse index: MCP agent ID → runtime agent ID.
    /// Enables resolving MCP IDs (e.g., "opencode@abcd1234") to runtime IDs
    /// (e.g., "%198") for message injection.
    mcp_index: Arc<RwLock<HashMap<String, String>>>,
    /// Queue of MCP agent IDs waiting to be bound to a runtime agent.
    /// Stores (mcp_agent_id, agent_identifier) tuples for precise binding.
    /// Populated when an MCP agent connects before rmux discovery finds panes.
    /// Drained after each successful discovery cycle.
    pending_mcp: Arc<RwLock<Vec<(String, String)>>>,
    /// Mutex to serialize binding operations.
    /// Ensures that even if multiple MCP agents connect concurrently,
    /// they are bound sequentially in creation-time order.
    binding_mutex: Arc<Mutex<()>>,
}

impl AgentRuntime {
    /// Create a new runtime with the given backend.
    pub fn new(backend: Arc<dyn AgentRuntimeBackend>) -> Self {
        Self {
            backend,
            registry: Arc::new(RwLock::new(HashMap::new())),
            mcp_index: Arc::new(RwLock::new(HashMap::new())),
            pending_mcp: Arc::new(RwLock::new(Vec::new())),
            binding_mutex: Arc::new(Mutex::new(())),
        }
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
            display_name: None,
            workspace_id: spec.id,
            handle: handle.clone(),
            state: AgentState::Running,
            task_id: None,
            created_at: chrono::Utc::now(),
            mcp_agent_id: None,
        };

        self.registry.write().await.insert(agent_id.clone(), info);
        self.spawn_monitor(agent_id.clone(), handle);

        info!(agent_id = agent_id, "Agent launched");
        Ok(agent_id)
    }

    /// Inject a message into a running agent.
    ///
    /// Uses the backend (rmux) to inject text directly into the agent's pane.
    /// Supports both runtime IDs (e.g., "%198") and MCP IDs (e.g., "opencode@abcd1234")
    /// — MCP IDs are resolved to runtime IDs via the `mcp_index` mapping.
    pub async fn inject_message(&self, agent_id: &str, message: &str) -> ErgataiResult<()> {
        // Resolve MCP ID to runtime ID if needed
        let runtime_id = self
            .resolve_agent_id(agent_id)
            .await
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;

        let info = {
            let registry = self.registry.read().await;
            registry
                .get(&runtime_id)
                .cloned()
                .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", runtime_id)))?
        };

        // Deliver via backend injection (rmux send_text)
        self.backend.inject_message(&info.handle, message).await
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

    /// Set the task ID for a runtime agent (for DAG tracking).
    pub async fn set_task_id(&self, agent_id: &str, task_id: String) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;
        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.task_id = Some(task_id);
        Ok(())
    }

    /// Set the display name for a runtime agent (for human-readable addressing).
    ///
    /// When set, agents can be addressed by this name in send_message instead of
    /// using the auto-generated ID (e.g., "%198"). Display names must be unique.
    /// Returns an error if the name is already taken by another agent.
    pub async fn set_display_name(
        &self,
        agent_id: &str,
        display_name: String,
    ) -> ErgataiResult<()> {
        let mut registry = self.registry.write().await;

        // Check if display_name is already taken by another agent
        let name_taken = registry.values().any(|info| {
            info.display_name.as_ref() == Some(&display_name) && info.agent_id != agent_id
        });
        if name_taken {
            return Err(ErgataiError::internal(format!(
                "Display name '{}' is already taken by another agent",
                display_name
            )));
        }

        let info = registry
            .get_mut(agent_id)
            .ok_or_else(|| ErgataiError::internal(format!("Agent {} not found", agent_id)))?;
        info.display_name = Some(display_name.clone());
        info!(agent_id = agent_id, display_name = %display_name, "Agent display name set");
        Ok(())
    }

    /// Find an agent by display name.
    ///
    /// Returns the agent_id if an agent with the given display_name exists.
    pub async fn find_agent_by_display_name(&self, display_name: &str) -> Option<String> {
        let registry = self.registry.read().await;
        registry
            .values()
            .find(|info| info.display_name.as_ref() == Some(&display_name.to_string()))
            .map(|info| info.agent_id.clone())
    }

    /// Register an externally-discovered agent (e.g., from rmux pane scan).
    ///
    /// This allows agents started outside the normal `launch_agent()` flow
    /// (e.g., manually in rmux panes) to receive messages via the runtime
    /// delivery chain.
    pub async fn register_discovered_agent(
        &self,
        agent_id: String,
        handle: AgentHandle,
    ) -> ErgataiResult<()> {
        let info = AgentInfo {
            agent_id: agent_id.clone(),
            display_name: None,
            workspace_id: handle.workspace.id.clone(),
            handle,
            state: AgentState::Running,
            task_id: None,
            created_at: chrono::Utc::now(),
            mcp_agent_id: None,
        };
        self.registry.write().await.insert(agent_id.clone(), info);
        debug!(agent_id = agent_id, "Registered discovered agent");
        Ok(())
    }

    /// Scan the backend for running agents and register any new ones.
    ///
    /// Returns the number of newly registered agents. Already-registered
    /// agents are skipped (idempotent).
    ///
    /// Each agent is registered atomically (single write-lock acquisition)
    /// to prevent TOCTOU races when this method is called concurrently
    /// (e.g., from the periodic re-discovery loop and manual triggers).
    pub async fn discover_and_register_agents(&self) -> ErgataiResult<usize> {
        let discovered = self.backend.discover_agents().await?;
        let mut count = 0;
        let mut registry = self.registry.write().await;
        for (agent_id, handle) in discovered {
            // Atomic check-and-insert under a single write lock acquisition.
            // entry().or_insert() ensures no TOCTOU gap between contains_key and insert.
            registry.entry(agent_id.clone()).or_insert_with(|| {
                count += 1;
                AgentInfo {
                    agent_id: agent_id.clone(),
                    display_name: None,
                    workspace_id: handle.workspace.id.clone(),
                    handle,
                    state: AgentState::Running,
                    task_id: None,
                    created_at: chrono::Utc::now(),
                    mcp_agent_id: None,
                }
            });
        }
        drop(registry);

        if count > 0 {
            info!(count = count, "Discovered and registered new agents");
        }

        // After discovery, try to bind pending MCP agents to newly discovered runtime agents
        if count > 0 {
            self.drain_pending_bindings().await;
        }

        Ok(count)
    }

    // ── MCP-to-Runtime agent ID binding ──

    /// Try to bind an MCP agent ID to an unmapped runtime agent.
    ///
    /// Uses FIFO strategy: finds the first runtime agent without an MCP binding
    /// and associates it with the given MCP ID. If no unmapped runtime agent
    /// exists, the MCP ID is added to the pending queue for later binding
    /// (when rmux discovery finds new panes).
    ///
    /// Returns the runtime agent ID if binding succeeded, or `None` if queued.
    pub async fn try_bind_mcp_agent(&self, mcp_agent_id: &str) -> Option<String> {
        // Acquire binding lock to serialize binding operations
        // This ensures that even with concurrent MCP connections,
        // bindings happen sequentially in creation-time order
        let _guard = self.binding_mutex.lock().await;

        // Check if already bound
        {
            let index = self.mcp_index.read().await;
            if let Some(runtime_id) = index.get(mcp_agent_id) {
                debug!(
                    mcp_agent_id = mcp_agent_id,
                    runtime_id = runtime_id,
                    "MCP agent already bound"
                );
                return Some(runtime_id.clone());
            }
        }

        // Sequential binding algorithm:
        // Find the FIRST unbound runtime agent (by discovery order)
        // This assumes panes are opened one at a time and MCP connects shortly after
        let mut registry = self.registry.write().await;
        let mut unbound_agents: Vec<_> = registry
            .values()
            .filter(|info| info.mcp_agent_id.is_none())
            .collect();

        if unbound_agents.is_empty() {
            // No unmapped runtime agent — add to pending queue
            drop(registry);
            let mut pending = self.pending_mcp.write().await;
            if !pending.iter().any(|(id, _)| id == mcp_agent_id) {
                pending.push((mcp_agent_id.to_string(), String::new()));
                info!(
                    mcp_agent_id = mcp_agent_id,
                    pending_count = pending.len(),
                    "No unmapped runtime agent, queued MCP agent for later binding"
                );
            }
            return None;
        }

        // Sort by creation time (earliest first) - sequential binding
        // The first unbound agent should match the first MCP connection
        unbound_agents.sort_by_key(|a| a.created_at);

        // Bind to the earliest unbound agent
        let matched_agent = unbound_agents.into_iter().next()?;
        let runtime_id = matched_agent.agent_id.clone();

        // Update the registry — agent may have been removed between sort and bind
        if let Some(info) = registry.get_mut(&runtime_id) {
            info.mcp_agent_id = Some(mcp_agent_id.to_string());
        } else {
            warn!(
                runtime_id = %runtime_id,
                mcp_agent_id = mcp_agent_id,
                "Agent disappeared during binding, aborting"
            );
            return None;
        }

        // Update the reverse index
        drop(registry);
        self.mcp_index
            .write()
            .await
            .insert(mcp_agent_id.to_string(), runtime_id.clone());

        info!(
            mcp_agent_id = mcp_agent_id,
            runtime_id = runtime_id,
            "Bound MCP agent to runtime agent (sequential algorithm with lock)"
        );
        Some(runtime_id)
    }

    /// Try to bind an MCP agent ID to a runtime agent using agent_identifier.
    ///
    /// This method matches MCP connections to runtime agents based on the
    /// ERGATAI_AGENT_ID environment variable set in startup scripts.
    /// Returns the runtime agent ID if binding succeeded.
    pub async fn try_bind_mcp_agent_with_identifier(
        &self,
        mcp_agent_id: &str,
        agent_identifier: &str,
    ) -> Option<String> {
        let _guard = self.binding_mutex.lock().await;

        // Check if already bound
        {
            let index = self.mcp_index.read().await;
            if let Some(runtime_id) = index.get(mcp_agent_id) {
                debug!(
                    mcp_agent_id = mcp_agent_id,
                    runtime_id = runtime_id,
                    "MCP agent already bound"
                );
                return Some(runtime_id.clone());
            }
        }

        // Find runtime agent with matching ergatai_agent_id
        let registry = self.registry.read().await;
        let matched_agent = registry.values().find(|info| {
            info.handle
                .metadata
                .get("ergatai_agent_id")
                .map(|id| id == agent_identifier)
                .unwrap_or(false)
        });

        let matched_agent = match matched_agent {
            Some(agent) => agent,
            None => {
                // No matching runtime agent found yet - add to pending queue
                // for later binding when discovery completes
                let mut pq = self.pending_mcp.write().await;
                if !pq.iter().any(|(id, _)| id == mcp_agent_id) {
                    info!(
                        mcp_agent_id = mcp_agent_id,
                        agent_identifier = agent_identifier,
                        "No runtime agent found yet, added to pending queue"
                    );
                    pq.push((mcp_agent_id.to_string(), agent_identifier.to_string()));
                } else {
                    debug!(
                        mcp_agent_id = mcp_agent_id,
                        agent_identifier = agent_identifier,
                        "Already in pending queue"
                    );
                }
                return None;
            }
        };

        let runtime_id = matched_agent.agent_id.clone();
        drop(registry);

        // Update the registry
        {
            let mut registry = self.registry.write().await;
            if let Some(info) = registry.get_mut(&runtime_id) {
                info.mcp_agent_id = Some(mcp_agent_id.to_string());
            }
        }

        // Update the reverse index
        self.mcp_index
            .write()
            .await
            .insert(mcp_agent_id.to_string(), runtime_id.clone());

        info!(
            mcp_agent_id = mcp_agent_id,
            runtime_id = runtime_id,
            agent_identifier = agent_identifier,
            "Bound MCP agent to runtime agent by identifier"
        );
        Some(runtime_id)
    }

    /// Drain the pending MCP queue by binding pending agents to newly discovered
    /// runtime agents. Called after `discover_and_register_agents` finds new agents.
    async fn drain_pending_bindings(&self) {
        let pending: Vec<(String, String)> = {
            let mut pq = self.pending_mcp.write().await;
            std::mem::take(&mut *pq)
        };

        if pending.is_empty() {
            return;
        }

        info!(
            pending_count = pending.len(),
            "Draining pending MCP bindings"
        );

        let mut registry = self.registry.write().await;
        let mut index = self.mcp_index.write().await;
        let mut bound = 0;
        let mut requeued = Vec::new();

        for (mcp_id, agent_identifier) in pending {
            // Skip if already bound (could happen if bound between queue and drain)
            if index.contains_key(&mcp_id) {
                continue;
            }

            // Find unbound runtime agents
            let mut unbound_agents: Vec<_> = registry
                .values()
                .filter(|info| info.mcp_agent_id.is_none())
                .collect();

            if unbound_agents.is_empty() {
                // Still no unmapped agent — re-queue
                requeued.push((mcp_id, agent_identifier));
                continue;
            }

            // Match by identifier if available, otherwise use FIFO
            let matched_agent = if !agent_identifier.is_empty() {
                // Find agent with matching ergatai_agent_id
                unbound_agents.into_iter().find(|info| {
                    info.handle
                        .metadata
                        .get("ergatai_agent_id")
                        .map(|id| id == &agent_identifier)
                        .unwrap_or(false)
                })
            } else {
                // FIFO: sort by creation time and take earliest
                unbound_agents.sort_by_key(|a| a.created_at);
                unbound_agents.into_iter().next()
            };

            let matched_agent = match matched_agent {
                Some(agent) => agent,
                None => {
                    // No matching agent found — re-queue
                    requeued.push((mcp_id, agent_identifier));
                    continue;
                }
            };

            let runtime_id = matched_agent.agent_id.clone();

            if let Some(info) = registry.get_mut(&runtime_id) {
                info.mcp_agent_id = Some(mcp_id.clone());
            }
            index.insert(mcp_id.clone(), runtime_id.clone());

            info!(
                mcp_agent_id = mcp_id,
                runtime_id = runtime_id,
                agent_identifier = agent_identifier,
                "Bound pending MCP agent to runtime agent"
            );
            bound += 1;
        }

        // Put back any that couldn't be bound
        if !requeued.is_empty() {
            let mut pq = self.pending_mcp.write().await;
            pq.extend(requeued);
        }

        if bound > 0 {
            info!(bound = bound, "Drained pending MCP bindings");
        }
    }

    /// Resolve any agent ID (MCP ID or runtime ID) to a runtime agent ID.
    ///
    /// First checks if the ID is a direct runtime ID, then checks the MCP index.
    pub async fn resolve_agent_id(&self, agent_id: &str) -> Option<String> {
        // Direct match in registry
        {
            let registry = self.registry.read().await;
            if registry.contains_key(agent_id) {
                return Some(agent_id.to_string());
            }
        }

        // MCP ID lookup
        let index = self.mcp_index.read().await;
        index.get(agent_id).cloned()
    }

    /// Get the MCP agent ID associated with a runtime agent.
    pub async fn get_mcp_agent_id(&self, runtime_id: &str) -> Option<String> {
        let registry = self.registry.read().await;
        registry
            .get(runtime_id)
            .and_then(|info| info.mcp_agent_id.clone())
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
    ///
    /// The monitor waits for the agent process to exit and updates the registry.
    /// A 24-hour safety timeout prevents leaked tasks if the backend hangs.
    fn spawn_monitor(&self, agent_id: String, handle: AgentHandle) {
        let backend = self.backend.clone();
        let registry = self.registry.clone();

        tokio::spawn(async move {
            // Safety timeout: 24 hours to prevent leaked tasks on hung backends
            let timeout_duration = std::time::Duration::from_secs(86400);

            let result =
                tokio::time::timeout(timeout_duration, backend.wait_for_exit(&handle, None)).await;

            match result {
                Ok(Ok(crate::types::WaitResult::Exited { code })) => {
                    info!(agent_id = agent_id, code = code, "Agent exited");
                }
                Ok(Ok(crate::types::WaitResult::Signaled { signal })) => {
                    warn!(
                        agent_id = agent_id,
                        signal = signal,
                        "Agent killed by signal"
                    );
                }
                Ok(Ok(crate::types::WaitResult::Timeout)) => {
                    warn!(agent_id = agent_id, "Agent monitor timed out (unexpected)");
                }
                Ok(Ok(crate::types::WaitResult::Error(e))) => {
                    error!(agent_id = agent_id, error = %e, "Agent monitor error");
                }
                Ok(Err(e)) => {
                    error!(agent_id = agent_id, error = %e, "Agent wait failed");
                }
                Err(_) => {
                    // tokio::time::timeout elapsed
                    warn!(
                        agent_id = agent_id,
                        "Agent monitor timed out after 24h, forcing Stopped state"
                    );
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
    use crate::types::{
        AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec,
    };
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
        let result = runtime
            .set_task_id("nonexistent", "task-42".to_string())
            .await;
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
    async fn test_inject_message_backend_failure_propagates() {
        // Backend injection fails → error propagates (no MCP fallback)
        let backend = Arc::new(MockBackend::with_inject_fail());
        let runtime = AgentRuntime::new(backend);
        let agent_id = runtime
            .launch_agent(make_spec("ws-1"), "cmd", None)
            .await
            .unwrap();

        let result = runtime.inject_message(&agent_id, "hello").await;
        assert!(result.is_err(), "backend failure should propagate as error");
    }
}
