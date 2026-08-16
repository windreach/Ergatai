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

    /// Return self as `&dyn Any` to enable downcasting through the trait object.
    ///
    /// Each concrete backend implements this as `fn as_any(&self) -> &dyn Any { self }`.
    /// Required because casting `&dyn AgentRuntimeBackend` directly to `&dyn Any` is
    /// not allowed (trait objects are not `Sized`).
    fn as_any(&self) -> &dyn std::any::Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentHandle, BackendCapabilities, ResourceLimits, WaitResult, WorkspaceHandle, WorkspaceSpec};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A mock backend for testing the trait interface.
    struct MockBackend {
        name: &'static str,
        create_count: AtomicUsize,
    }

    impl MockBackend {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                create_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl AgentRuntimeBackend for MockBackend {
        fn name(&self) -> &'static str {
            self.name
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_message_injection: true,
                supports_output_capture: false,
                supports_resource_limits: false,
                supports_workspace_reuse: false,
                supports_network_isolation: false,
                max_concurrent_agents: None,
            }
        }

        async fn initialize(&self) -> ErgataiResult<()> {
            Ok(())
        }

        async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle> {
            self.create_count.fetch_add(1, Ordering::SeqCst);
            Ok(WorkspaceHandle {
                id: spec.id,
                backend: self.name.to_string(),
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
                agent_id: format!("{}-agent", handle.id),
                process_id: None,
                metadata: HashMap::new(),
            })
        }

        async fn inject_message(&self, _handle: &AgentHandle, _message: &str) -> ErgataiResult<()> {
            Ok(())
        }

        async fn capture_output(&self, _handle: &AgentHandle) -> ErgataiResult<Option<String>> {
            Ok(None)
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

    #[test]
    fn test_backend_name() {
        let backend = MockBackend::new("mock");
        assert_eq!(backend.name(), "mock");
    }

    #[test]
    fn test_backend_capabilities() {
        let backend = MockBackend::new("mock");
        let caps = backend.capabilities();
        assert!(caps.supports_message_injection);
        assert!(!caps.supports_output_capture);
        assert!(caps.max_concurrent_agents.is_none());
    }

    #[tokio::test]
    async fn test_backend_initialize() {
        let backend = MockBackend::new("mock");
        assert!(backend.initialize().await.is_ok());
    }

    #[tokio::test]
    async fn test_backend_create_workspace() {
        let backend = MockBackend::new("mock");
        let spec = WorkspaceSpec {
            id: "ws-1".to_string(),
            work_dir: PathBuf::from("/tmp"),
            env: HashMap::new(),
            resources: ResourceLimits::default(),
            backend_config: serde_json::json!({}),
        };
        let handle = backend.create_workspace(spec).await.unwrap();
        assert_eq!(handle.id, "ws-1");
        assert_eq!(handle.backend, "mock");
        assert_eq!(backend.create_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_backend_start_agent() {
        let backend = MockBackend::new("mock");
        let ws = WorkspaceHandle {
            id: "ws-1".to_string(),
            backend: "mock".to_string(),
            metadata: HashMap::new(),
        };
        let agent = backend.start_agent(&ws, "echo hi", None).await.unwrap();
        assert_eq!(agent.agent_id, "ws-1-agent");
        assert_eq!(agent.workspace.id, "ws-1");
    }

    #[tokio::test]
    async fn test_backend_inject_message() {
        let backend = MockBackend::new("mock");
        let agent = AgentHandle {
            workspace: WorkspaceHandle {
                id: "ws-1".to_string(),
                backend: "mock".to_string(),
                metadata: HashMap::new(),
            },
            agent_id: "agent-1".to_string(),
            process_id: None,
            metadata: HashMap::new(),
        };
        assert!(backend.inject_message(&agent, "hello").await.is_ok());
    }

    #[tokio::test]
    async fn test_backend_wait_for_exit() {
        let backend = MockBackend::new("mock");
        let agent = AgentHandle {
            workspace: WorkspaceHandle {
                id: "ws-1".to_string(),
                backend: "mock".to_string(),
                metadata: HashMap::new(),
            },
            agent_id: "agent-1".to_string(),
            process_id: None,
            metadata: HashMap::new(),
        };
        let result = backend.wait_for_exit(&agent, None).await.unwrap();
        match result {
            WaitResult::Exited { code } => assert_eq!(code, 0),
            _ => panic!("Expected Exited"),
        }
    }

    #[tokio::test]
    async fn test_backend_shutdown() {
        let backend = MockBackend::new("mock");
        assert!(backend.shutdown().await.is_ok());
    }

    #[test]
    fn test_backend_as_any_downcast() {
        let backend = MockBackend::new("mock");
        let trait_obj: &dyn AgentRuntimeBackend = &backend;
        let downcast = trait_obj.as_any().downcast_ref::<MockBackend>();
        assert!(downcast.is_some());
        assert_eq!(downcast.unwrap().name(), "mock");
    }

    #[tokio::test]
    async fn test_backend_list_workspaces_empty() {
        let backend = MockBackend::new("mock");
        let workspaces = backend.list_workspaces().await.unwrap();
        assert!(workspaces.is_empty());
    }
}
