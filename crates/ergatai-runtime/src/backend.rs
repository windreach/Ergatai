//! Agent Runtime Backend trait — the core abstraction for pluggable execution environments.
//!
//! Implementations of this trait provide the actual mechanism for creating workspaces,
//! launching agents, injecting messages, and capturing output. The runtime facade
//! (`AgentRuntime`) delegates all backend-specific operations to the selected implementation.

use std::time::Duration;

use async_trait::async_trait;

use ergatai_error::ErgataiResult;

use crate::types::{AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec};

/// Pluggable execution backend for agents.
///
/// Each backend manages a specific type of execution environment:
/// - **LocalPtyBackend**: tmux sessions and panes (current default)
/// - **DirectProcessBackend**: direct process spawning (no terminal)
/// - **DockerBackend**: Docker containers (future)
/// - **RemoteSSHBackend**: SSH to remote hosts (future)
/// - **KubernetesBackend**: K8s pods (future)
///
/// # Design Principles
///
/// 1. **Backend Agnostic** — trait defines *what*, not *how*
/// 2. **Capability-Based** — backends declare what they support via `capabilities()`
/// 3. **Fail-Graceful** — unsupported operations return `Err`, callers degrade
/// 4. **Async-First** — all operations are async (network, process, container APIs)
#[async_trait]
pub trait AgentRuntimeBackend: Send + Sync + 'static {
    /// Human-readable backend name (e.g., "local-pty", "docker", "ssh").
    fn name(&self) -> &'static str;

    /// Declare what this backend can do.
    fn capabilities(&self) -> BackendCapabilities;

    /// Initialize the backend (check dependencies, create resources).
    async fn initialize(&self) -> ErgataiResult<()>;

    /// Create a workspace for an agent.
    async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle>;

    /// Start an agent in the workspace.
    async fn start_agent(
        &self,
        handle: &WorkspaceHandle,
        command: &str,
        instruction: Option<&str>,
    ) -> ErgataiResult<AgentHandle>;

    /// Inject a message into a running agent.
    async fn inject_message(&self, handle: &AgentHandle, message: &str) -> ErgataiResult<()>;

    /// Capture agent output for result collection.
    async fn capture_output(&self, handle: &AgentHandle) -> ErgataiResult<Option<String>>;

    /// Check if an agent is still running.
    async fn is_alive(&self, handle: &AgentHandle) -> ErgataiResult<bool>;

    /// Stop an agent gracefully.
    async fn stop_agent(&self, handle: &AgentHandle) -> ErgataiResult<()>;

    /// Force-kill an agent (no graceful shutdown).
    async fn kill_agent(&self, handle: &AgentHandle) -> ErgataiResult<()>;

    /// Wait for an agent to exit.
    async fn wait_for_exit(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult>;

    /// List all active workspaces (for cleanup and recovery).
    async fn list_workspaces(&self) -> ErgataiResult<Vec<WorkspaceHandle>>;

    /// Cleanup a workspace (remove resources).
    async fn cleanup_workspace(&self, handle: &WorkspaceHandle) -> ErgataiResult<()>;

    /// Cleanup all resources (shutdown).
    async fn shutdown(&self) -> ErgataiResult<()>;
}
