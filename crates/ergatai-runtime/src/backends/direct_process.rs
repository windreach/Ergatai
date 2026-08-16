//! DirectProcessBackend — spawn agent processes directly without a terminal multiplexer.
//!
//! This backend is simpler than LocalPtyBackend: it spawns processes directly,
//! captures stdout/stderr, and tracks them by PID. No message injection support
//! (agents must use MCP notifications for communication).
//!
//! Best for: headless environments (CI/CD, containers), testing, and deployments
//! where tmux is not available.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::{debug, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use crate::backend::AgentRuntimeBackend;
use crate::types::{AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec};

/// Poll interval for checking process liveness.
const EXIT_POLL_INTERVAL: Duration = Duration::from_secs(1);

// ── DirectProcessBackend ──

/// Exit code slot for a process, shared between the backend and monitor tasks.
type ExitCodeSlot = Arc<tokio::sync::Mutex<Option<i32>>>;

/// Direct process execution backend (no terminal multiplexer).
pub struct DirectProcessBackend {
    /// Base directory for workspace directories.
    work_dir_base: std::path::PathBuf,
    /// Exit code slots keyed by PID string — populated by monitor tasks when processes exit.
    exit_codes: Arc<tokio::sync::Mutex<HashMap<String, ExitCodeSlot>>>,
}

impl DirectProcessBackend {
    /// Create a new backend with the given base working directory.
    pub fn new(work_dir_base: std::path::PathBuf) -> Self {
        Self {
            work_dir_base,
            exit_codes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl AgentRuntimeBackend for DirectProcessBackend {
    fn name(&self) -> &'static str {
        "direct-process"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_message_injection: false,
            supports_output_capture: true,
            supports_resource_limits: false,
            supports_workspace_reuse: false,
            supports_network_isolation: false,
            max_concurrent_agents: None,
        }
    }

    async fn initialize(&self) -> ErgataiResult<()> {
        tokio::fs::create_dir_all(&self.work_dir_base)
            .await
            .map_err(|e| {
                ErgataiError::internal(format!(
                    "Failed to create work dir base {}: {}",
                    self.work_dir_base.display(),
                    e
                ))
            })?;
        info!(
            path = %self.work_dir_base.display(),
            "DirectProcessBackend initialized"
        );
        Ok(())
    }

    async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle> {
        let workspace_dir = self.work_dir_base.join(&spec.id);
        tokio::fs::create_dir_all(&workspace_dir)
            .await
            .map_err(|e| {
                ErgataiError::internal(format!(
                    "Failed to create workspace dir {}: {}",
                    workspace_dir.display(),
                    e
                ))
            })?;

        let mut metadata = HashMap::new();
        metadata.insert(
            "work_dir".to_string(),
            workspace_dir.to_string_lossy().to_string(),
        );

        info!(id = spec.id, dir = %workspace_dir.display(), "Workspace created");

        Ok(WorkspaceHandle {
            id: spec.id,
            backend: "direct-process".to_string(),
            metadata,
        })
    }

    async fn start_agent(
        &self,
        handle: &WorkspaceHandle,
        command: &str,
        instruction: Option<&str>,
    ) -> ErgataiResult<AgentHandle> {
        let work_dir = handle
            .metadata
            .get("work_dir")
            .ok_or_else(|| ErgataiError::internal("Missing work_dir in workspace handle"))?;

        let mut parts = command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| ErgataiError::internal("Empty command"))?;
        let args: Vec<&str> = parts.collect();

        debug!(
            program = program,
            args = ?args,
            work_dir = work_dir,
            "Spawning agent process"
        );

        let mut child = tokio::process::Command::new(program)
            .args(&args)
            .current_dir(work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to spawn process '{}': {}", program, e))
            })?;

        let pid = child
            .id()
            .ok_or_else(|| ErgataiError::internal("Failed to get PID from spawned process"))?;

        if let Some(instr) = instruction {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(instr.as_bytes()).await.map_err(|e| {
                    ErgataiError::internal(format!("Failed to write instruction to stdin: {}", e))
                })?;
                stdin.write_all(b"\n").await.ok();
                drop(stdin);
            }
        }

        // Spawn a monitor task that holds the Child handle and waits for exit,
        // recording the real exit code in a shared slot. This prevents zombies
        // and allows wait_for_exit to report the actual exit code.
        let exit_code = Arc::new(tokio::sync::Mutex::new(None));
        let exit_code_clone = exit_code.clone();
        let agent_id_for_monitor = format!("agent-{}", uuid::Uuid::new_v4());
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            *exit_code_clone.lock().await = Some(code);
            debug!(pid = pid, exit_code = code, "Process exited");
        });

        let agent_id = agent_id_for_monitor;

        // Register the exit code slot so wait_for_exit can retrieve the real code
        self.exit_codes
            .lock()
            .await
            .insert(pid.to_string(), exit_code);

        info!(pid = pid, agent_id = agent_id, "Agent process started");

        Ok(AgentHandle {
            workspace: handle.clone(),
            agent_id,
            process_id: Some(pid.to_string()),
            metadata: HashMap::new(),
        })
    }

    async fn inject_message(&self, _handle: &AgentHandle, _message: &str) -> ErgataiResult<()> {
        Err(ErgataiError::internal(
            "DirectProcessBackend does not support message injection. \
             Use MCP notifications instead."
                .to_string(),
        ))
    }

    async fn capture_output(&self, _handle: &AgentHandle) -> ErgataiResult<Option<String>> {
        debug!("capture_output not fully implemented for DirectProcessBackend");
        Ok(None)
    }

    async fn is_alive(&self, handle: &AgentHandle) -> ErgataiResult<bool> {
        let pid = handle
            .process_id
            .as_ref()
            .ok_or_else(|| ErgataiError::internal("Missing PID in agent handle"))?;

        #[cfg(unix)]
        {
            let result = tokio::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .await
                .map_err(|e| {
                    ErgataiError::internal(format!("Failed to check process {}: {}", pid, e))
                })?;
            Ok(result.success())
        }

        #[cfg(not(unix))]
        {
            let _ = pid;
            Ok(false)
        }
    }

    async fn stop_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        let pid = handle
            .process_id
            .as_ref()
            .ok_or_else(|| ErgataiError::internal("Missing PID in agent handle"))?;

        info!(pid = pid, "Sending SIGTERM to agent process");

        #[cfg(unix)]
        {
            tokio::process::Command::new("kill")
                .args(["-TERM", pid])
                .status()
                .await
                .map_err(|e| {
                    ErgataiError::internal(format!("Failed to send SIGTERM to {}: {}", pid, e))
                })?;

            // Wait up to 5 seconds for the process to exit gracefully.
            // If it doesn't exit, the caller can escalate via kill_agent() (SIGKILL).
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if !self.is_alive(handle).await.unwrap_or(false) {
                    info!(pid = pid, "Agent process exited after SIGTERM");
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            warn!(
                pid = pid,
                "Agent process did not exit within 5s after SIGTERM"
            );
        }

        Ok(())
    }

    async fn kill_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        let pid = handle
            .process_id
            .as_ref()
            .ok_or_else(|| ErgataiError::internal("Missing PID in agent handle"))?;

        warn!(pid = pid, "Sending SIGKILL to agent process");

        #[cfg(unix)]
        {
            tokio::process::Command::new("kill")
                .args(["-KILL", pid])
                .status()
                .await
                .map_err(|e| {
                    ErgataiError::internal(format!("Failed to send SIGKILL to {}: {}", pid, e))
                })?;
        }

        Ok(())
    }

    async fn wait_for_exit(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult> {
        let start = Instant::now();

        loop {
            if !self.is_alive(handle).await? {
                // Retrieve the real exit code from the monitor task's slot
                let code = if let Some(pid) = &handle.process_id {
                    let codes = self.exit_codes.lock().await;
                    codes
                        .get(pid)
                        .and_then(|slot| {
                            // Try to read without blocking — if the monitor hasn't
                            // recorded the code yet, it will shortly
                            slot.try_lock().ok().and_then(|guard| *guard)
                        })
                        .unwrap_or(-1)
                } else {
                    -1
                };
                return Ok(WaitResult::Exited { code });
            }

            if let Some(timeout) = timeout {
                if start.elapsed() > timeout {
                    return Ok(WaitResult::Timeout);
                }
            }

            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    }

    async fn list_workspaces(&self) -> ErgataiResult<Vec<WorkspaceHandle>> {
        let mut workspaces = Vec::new();

        if !self.work_dir_base.exists() {
            return Ok(workspaces);
        }

        let mut entries = tokio::fs::read_dir(&self.work_dir_base)
            .await
            .map_err(|e| {
                ErgataiError::internal(format!(
                    "Failed to read work dir {}: {}",
                    self.work_dir_base.display(),
                    e
                ))
            })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to read dir entry: {}", e)))?
        {
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                let id = entry.file_name().to_string_lossy().to_string();
                let mut metadata = HashMap::new();
                metadata.insert(
                    "work_dir".to_string(),
                    entry.path().to_string_lossy().to_string(),
                );
                workspaces.push(WorkspaceHandle {
                    id,
                    backend: "direct-process".to_string(),
                    metadata,
                });
            }
        }

        Ok(workspaces)
    }

    async fn cleanup_workspace(&self, handle: &WorkspaceHandle) -> ErgataiResult<()> {
        let work_dir = handle
            .metadata
            .get("work_dir")
            .ok_or_else(|| ErgataiError::internal("Missing work_dir in workspace handle"))?;

        info!(dir = work_dir, "Cleaning up workspace directory");

        tokio::fs::remove_dir_all(work_dir).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to remove workspace dir {}: {}",
                work_dir, e
            ))
        })?;

        Ok(())
    }

    async fn shutdown(&self) -> ErgataiResult<()> {
        let workspaces = self.list_workspaces().await?;
        for ws in &workspaces {
            if let Err(e) = self.cleanup_workspace(ws).await {
                warn!(error = %e, workspace = ws.id, "Failed to cleanup workspace during shutdown");
            }
        }
        info!(
            count = workspaces.len(),
            "DirectProcessBackend shutdown complete"
        );
        Ok(())
    }
}
