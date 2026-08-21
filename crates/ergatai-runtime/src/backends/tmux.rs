//! TmuxBackend — tmux-based agent execution environment.
//!
//! Agents run in tmux panes, messages are injected via `send-keys -l`,
//! and output is captured via `capture-pane`.
//!
//! # Event-driven features
//!
//! - **wait_for_exit**: uses tmux `pane-died` hook + `wait-for` for efficient exit detection
//! - **Agent discovery**: reads `TMUX_PANE` env from `/proc/{pid}/environ` for deterministic IDs
//! - **Health check**: reads `/proc/{pid}/stat` via shared `proc_linux` module

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use tracing::{debug, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use crate::backend::AgentRuntimeBackend;
use crate::types::{AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec};

// ── Configuration constants ──

/// Maximum message size for tmux injection (64 KiB).
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Timeout for individual tmux commands.
const TMUX_CMD_TIMEOUT: Duration = Duration::from_secs(10);

/// Number of retries for transient failures.
const TMUX_CMD_RETRIES: u32 = 2;

/// Delay between retries.
const TMUX_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Default terminal dimensions.
const DEFAULT_WIDTH: u32 = 200;
const DEFAULT_HEIGHT: u32 = 50;

/// Delay before injecting instructions after agent start.
const INSTRUCTION_DELAY: Duration = Duration::from_secs(2);

/// Poll interval for `wait_for_exit`.
const EXIT_POLL_INTERVAL: Duration = Duration::from_secs(5);

// ── TmuxBackend ──

/// tmux-based agent execution backend.
///
/// Each workspace is a tmux session. Each agent is a pane within that session.
/// Session names follow the pattern `{prefix}-{workspace_id}`.
pub struct TmuxBackend {
    /// Session name prefix (e.g., "ergatai" or "ergatai-opencode")
    session_prefix: String,
    /// Default terminal dimensions
    width: u32,
    height: u32,
}

impl TmuxBackend {
    /// Create a new backend with the given session prefix.
    pub fn new(session_prefix: &str) -> Self {
        Self {
            session_prefix: session_prefix.to_string(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        }
    }

    /// Create with custom dimensions.
    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Build the full session name from prefix + workspace ID.
    fn session_name(&self, workspace_id: &str) -> String {
        let safe_id = workspace_id.replace(['|', ':', '.'], "-");
        format!("{}-{}", self.session_prefix, safe_id)
    }

    /// Run a tmux command with timeout and retry logic.
    async fn run_tmux_cmd(args: &[&str]) -> ErgataiResult<std::process::Output> {
        let mut last_err = None;
        for attempt in 0..=TMUX_CMD_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(TMUX_RETRY_DELAY).await;
                debug!(
                    "Retrying tmux command (attempt {}): {:?}",
                    attempt + 1,
                    args
                );
            }

            let result = tokio::time::timeout(
                TMUX_CMD_TIMEOUT,
                tokio::process::Command::new("tmux").args(args).output(),
            )
            .await;

            match result {
                Ok(Ok(output)) => return Ok(output),
                Ok(Err(e)) => {
                    last_err = Some(ErgataiError::internal(format!("tmux exec failed: {}", e)));
                }
                Err(_) => {
                    last_err = Some(ErgataiError::internal(format!(
                        "tmux command timed out after {:?}: {:?}",
                        TMUX_CMD_TIMEOUT, args
                    )));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ErgataiError::internal("tmux command failed")))
    }

    /// Run a tmux command and check for success.
    async fn run_tmux_cmd_checked(args: &[&str], context_msg: &str) -> ErgataiResult<()> {
        let output = Self::run_tmux_cmd(args).await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ErgataiError::internal(format!(
                "{}: {}",
                context_msg,
                stderr.trim()
            )));
        }
        Ok(())
    }

    /// Send sanitized text + Enter to a tmux pane.
    async fn send_to_pane(pane: &str, text: &str) -> ErgataiResult<()> {
        let sanitized = sanitize_message(text);

        Self::run_tmux_cmd_checked(
            &["send-keys", "-l", "-t", pane, &sanitized],
            "Failed to inject text via tmux",
        )
        .await?;

        // Delay to let the terminal process the text before sending Enter.
        // Matches rmux behaviour — without this the agent may still be handling
        // the previous keystroke when Enter arrives, causing the message to be
        // swallowed or split across inputs.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Self::run_tmux_cmd_checked(
            &["send-keys", "-t", pane, "Enter"],
            "Failed to send Enter via tmux",
        )
        .await?;

        Ok(())
    }

    /// Extract pane_id from agent handle metadata.
    fn pane_id(handle: &AgentHandle) -> ErgataiResult<String> {
        handle
            .metadata
            .get("pane_id")
            .cloned()
            .ok_or_else(|| ErgataiError::internal("Missing pane_id in agent handle metadata"))
    }

    /// Extract session from workspace handle metadata.
    fn session_name_from_handle(handle: &WorkspaceHandle) -> ErgataiResult<String> {
        handle
            .metadata
            .get("session")
            .cloned()
            .ok_or_else(|| ErgataiError::internal("Missing session in workspace handle metadata"))
    }

    /// Polling fallback for wait_for_exit (used when event-driven path fails).
    async fn wait_for_exit_poll(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult> {
        let start = Instant::now();
        loop {
            match self.is_alive(handle).await {
                Ok(false) => return Ok(WaitResult::Exited { code: 0 }),
                Ok(true) => {}
                Err(e) => return Ok(WaitResult::Error(e.to_string())),
            }
            if let Some(t) = timeout {
                if start.elapsed() > t {
                    return Ok(WaitResult::Timeout);
                }
            }
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    }

    /// Check health of all agent processes running in tmux panes.
    ///
    /// Reads `/proc/{pid}/stat` for each pane's child process to detect
    /// Zombie/Dead states. Called by `AgentRuntime::prune_unhealthy_agents()`.
    pub async fn health_check_agents(
        &self,
    ) -> Vec<(String, super::proc_linux::ProcessState)> {
        use super::proc_linux::read_proc_state;

        let sessions_output = match Self::run_tmux_cmd(&[
            "list-sessions", "-F", "#{session_name}",
        ])
        .await
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };

        let stdout = String::from_utf8_lossy(&sessions_output.stdout);
        let prefix = format!("{}-", self.session_prefix);
        let sessions: Vec<String> = stdout
            .lines()
            .filter(|l| l.starts_with(&prefix))
            .map(|l| l.to_string())
            .collect();

        let mut results = Vec::new();

        for session in &sessions {
            let pane_output = match Self::run_tmux_cmd(&[
                "list-panes", "-t", session,
                "-F", "#{pane_id}|#{pane_pid}",
            ])
            .await
            {
                Ok(o) if o.status.success() => o,
                _ => continue,
            };

            let pane_stdout = String::from_utf8_lossy(&pane_output.stdout);
            for line in pane_stdout.lines() {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if parts.len() < 2 {
                    continue;
                }
                let pane_id = parts[0];
                let pid: u32 = match parts[1].parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                // Use pane_id as agent_id for health check identification
                let agent_id = read_tmux_pane_env(pid)
                    .unwrap_or_else(|| format!("pane_{}", pane_id.replace('%', "")));

                #[cfg(target_os = "linux")]
                let state = read_proc_state(pid).unwrap_or(super::proc_linux::ProcessState::Unknown);
                #[cfg(not(target_os = "linux"))]
                let state = super::proc_linux::ProcessState::Unknown;

                results.push((agent_id, state));
            }
        }

        results
    }

    /// Get tmux server status (version, sessions, pane counts).
    ///
    /// Called by the `/api/v1/status` endpoint via downcast.
    pub async fn tmux_status(&self) -> TmuxStatus {
        // Get version
        let version = match Self::run_tmux_cmd(&["-V"]).await {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => "unknown".to_string(),
        };

        // List sessions with pane counts
        let sessions_output = match Self::run_tmux_cmd(&[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_panels}|#{session_created_string}",
        ])
        .await
        {
            Ok(o) if o.status.success() => o,
            _ => {
                return TmuxStatus {
                    version,
                    sessions: Vec::new(),
                    total_panes: 0,
                };
            }
        };

        let stdout = String::from_utf8_lossy(&sessions_output.stdout);
        let mut sessions = Vec::new();
        let mut total_panes = 0;

        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }
            let panes: usize = parts[1].parse().unwrap_or(0);
            total_panes += panes;
            sessions.push(TmuxSessionInfo {
                name: parts[0].to_string(),
                panes,
                created: parts[2].to_string(),
            });
        }

        TmuxStatus {
            version,
            sessions,
            total_panes,
        }
    }
}

/// Snapshot of tmux server state — returned by `TmuxBackend::tmux_status()`.
#[derive(Debug, Clone, Serialize)]
pub struct TmuxStatus {
    /// tmux version string (e.g., "3.4")
    pub version: String,
    /// Active tmux sessions
    pub sessions: Vec<TmuxSessionInfo>,
    /// Total number of panes across all sessions
    pub total_panes: usize,
}

/// Information about a single tmux session.
#[derive(Debug, Clone, Serialize)]
pub struct TmuxSessionInfo {
    pub name: String,
    pub panes: usize,
    pub created: String,
}

#[async_trait]
impl AgentRuntimeBackend for TmuxBackend {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_message_injection: true,
            supports_output_capture: true,
            supports_resource_limits: false,
            supports_workspace_reuse: true,
            supports_network_isolation: false,
            max_concurrent_agents: None,
        }
    }

    async fn initialize(&self) -> ErgataiResult<()> {
        let output = Self::run_tmux_cmd(&["-V"]).await?;
        if !output.status.success() {
            return Err(ErgataiError::internal(
                "tmux -V exited with non-zero status".to_string(),
            ));
        }
        let version = String::from_utf8_lossy(&output.stdout);
        info!("tmux version: {}", version.trim());
        Ok(())
    }

    async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle> {
        let session_name = self.session_name(&spec.id);

        info!(
            session = session_name,
            width = self.width,
            height = self.height,
            "Creating tmux session workspace"
        );

        // Capture pane_id from new-session output
        let result = Self::run_tmux_cmd(&[
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-x",
            &self.width.to_string(),
            "-y",
            &self.height.to_string(),
            "-P",
            "-F",
            "#{pane_id}",
        ])
        .await;

        let mut metadata = HashMap::new();
        metadata.insert("session".to_string(), session_name.clone());

        match result {
            Ok(output) => {
                let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                metadata.insert("default_pane_id".to_string(), pane_id);
            }
            Err(e) => {
                let err_str = e.to_string();
                if !err_str.contains("duplicate") && !err_str.contains("already exists") {
                    return Err(e);
                }
                debug!("Session {} already exists, reusing", session_name);
                // For reused sessions, find the first pane
                if let Ok(output) = Self::run_tmux_cmd(&[
                    "list-panes",
                    "-t",
                    &session_name,
                    "-F",
                    "#{pane_id}",
                ]).await {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    if let Some(pane_id) = stdout.lines().next().map(|l| l.trim().to_string()) {
                        metadata.insert("default_pane_id".to_string(), pane_id);
                    }
                }
            }
        }

        // Store persist flag; destroy-unattached will be set after start_agent
        let persist = spec.backend_config.get("persist")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        metadata.insert("persist".to_string(), persist.to_string());

        Ok(WorkspaceHandle {
            id: spec.id,
            backend: "tmux".to_string(),
            metadata,
        })
    }

    async fn start_agent(
        &self,
        handle: &WorkspaceHandle,
        command: &str,
        instruction: Option<&str>,
    ) -> ErgataiResult<AgentHandle> {
        let session = Self::session_name_from_handle(handle)?;

        if command.is_empty() {
            return Err(ErgataiError::internal(
                "Agent command must not be empty".to_string(),
            ));
        }
        if command.contains('\n') || command.contains('\r') {
            return Err(ErgataiError::internal(
                "Agent command must not contain newlines".to_string(),
            ));
        }

        // Use the stored default pane ID from workspace metadata
        let pane_id = handle
            .metadata
            .get("default_pane_id")
            .cloned()
            .ok_or_else(|| ErgataiError::internal("Missing default_pane_id in workspace metadata"))?;

        let persist = handle.metadata.get("persist")
            .map(|v| v == "true")
            .unwrap_or(false);

        // Non-persist: exec replaces shell with agent process
        //   → agent exits → pane dies → pane-died hook → session destroyed
        // Persist: keep shell alive after agent exits so user can continue working
        let send_command = if persist {
            command.to_string()
        } else {
            format!("exec bash -c '{}'", command.replace('\'', "'\\''"))
        };

        Self::run_tmux_cmd_checked(
            &["send-keys", "-l", "-t", &pane_id, &send_command],
            "Failed to send command to pane",
        )
        .await?;

        // Delay to let the terminal process the command text before Enter.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        Self::run_tmux_cmd_checked(
            &["send-keys", "-t", &pane_id, "Enter"],
            "Failed to send Enter to pane",
        )
        .await?;

        info!(
            pane_id = pane_id,
            session = session,
            "Agent started in tmux pane"
        );

        if let Some(instr) = instruction {
            tokio::time::sleep(INSTRUCTION_DELAY).await;
            Self::send_to_pane(&pane_id, instr).await?;
            info!(pane_id = pane_id, "Instruction injected ({}B)", instr.len());
        }

        // Set auto-kill hooks only for non-persist sessions
        if !persist {
            // Kill session when user detaches
            let _ = Self::run_tmux_cmd(&[
                "set-hook",
                "-t",
                &session,
                "client-detached",
                "kill-session -t '#S'",
            ]).await;
            // Kill session when agent process exits (pane dies)
            let _ = Self::run_tmux_cmd(&[
                "set-hook",
                "-t",
                &session,
                "pane-died",
                "kill-session -t '#S'",
            ]).await;
        }

        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());
        let mut metadata = HashMap::new();
        metadata.insert("pane_id".to_string(), pane_id.clone());
        metadata.insert("session".to_string(), session.clone());
        // Store workspace ID as ergatai_agent_id initially.
        // discover_agents() will later read ERGATAI_AGENT_ID from the child
        // process and update this to the correct MCP binding identifier.
        metadata.insert("ergatai_agent_id".to_string(), handle.id.clone());

        Ok(AgentHandle {
            workspace: handle.clone(),
            agent_id,
            process_id: None,
            metadata,
        })
    }

    async fn inject_message(&self, handle: &AgentHandle, message: &str) -> ErgataiResult<()> {
        let pane_id = Self::pane_id(handle)?;
        Self::send_to_pane(&pane_id, message).await
    }

    async fn capture_output(&self, handle: &AgentHandle) -> ErgataiResult<Option<String>> {
        let pane_id = Self::pane_id(handle)?;

        let output = Self::run_tmux_cmd(&["capture-pane", "-t", &pane_id, "-p"]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ErgataiError::internal(format!(
                "Failed to capture pane: {}",
                stderr.trim()
            )));
        }

        let raw = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(Some(strip_ansi(&raw)))
    }

    async fn is_alive(&self, handle: &AgentHandle) -> ErgataiResult<bool> {
        let pane_id = Self::pane_id(handle)?;
        let result = Self::run_tmux_cmd(&["list-panes", "-t", &pane_id]).await;
        Ok(result.map(|o| o.status.success()).unwrap_or(false))
    }

    async fn stop_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        let pane_id = Self::pane_id(handle)?;
        info!(pane_id = pane_id, "Stopping agent (killing pane)");

        if let Err(e) =
            Self::run_tmux_cmd_checked(&["kill-pane", "-t", &pane_id], "Failed to kill pane").await
        {
            warn!(pane_id = pane_id, error = %e, "Failed to kill pane (may already be closed)");
        }

        Ok(())
    }

    async fn kill_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        self.stop_agent(handle).await
    }

    async fn wait_for_exit(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult> {
        let pane_id = Self::pane_id(handle)?;
        let channel = format!("ergatai-exit-{}", pane_id.replace('%', "-"));

        // 1. Install global pane-died hook that signals the per-pane channel.
        //    The hook uses #{pane_id} which tmux expands at fire time.
        //    set-hook is idempotent — re-setting replaces the previous hook.
        //    We use a single hook that signals ALL pane deaths; the channel
        //    name encodes the pane_id so only the correct waiter is woken.
        Self::run_tmux_cmd(&[
            "set-hook",
            "-g",
            "pane-died",
            &format!(
                "run-shell \"tmux wait-for -S ergatai-exit-#{pane_id} 2>/dev/null || true\""
            ),
        ])
        .await?;

        // 2. Check if already dead (handles race: pane died before hook was set)
        match self.is_alive(handle).await {
            Ok(false) => return Ok(WaitResult::Exited { code: 0 }),
            Ok(true) => {}
            Err(e) => return Ok(WaitResult::Error(e.to_string())),
        }

        // 3. Block on tmux wait-for (event-driven, no polling)
        let effective_timeout = timeout.unwrap_or(Duration::from_secs(3600));
        let result = tokio::time::timeout(
            effective_timeout,
            Self::run_tmux_cmd(&["wait-for", &channel]),
        )
        .await;

        match result {
            Ok(Ok(_)) => Ok(WaitResult::Exited { code: 0 }),
            Ok(Err(e)) => {
                warn!(pane_id = pane_id, error = %e, "wait-for failed, falling back to poll");
                // Fallback: poll if wait-for itself failed
                self.wait_for_exit_poll(handle, timeout).await
            }
            Err(_) => Ok(WaitResult::Timeout),
        }
    }

    async fn list_workspaces(&self) -> ErgataiResult<Vec<WorkspaceHandle>> {
        let output = Self::run_tmux_cmd(&["list-sessions", "-F", "#{session_name}"]).await?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let prefix = format!("{}-", self.session_prefix);

        let session_names: Vec<String> = stdout
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .map(|s| s.to_string())
            .collect();

        let mut workspaces = Vec::new();
        for session_name in &session_names {
            let id = session_name
                .strip_prefix(&prefix)
                .unwrap_or(session_name)
                .to_string();
            let mut metadata = HashMap::new();
            metadata.insert("session".to_string(), session_name.clone());
            // Find first pane for this session
            if let Ok(pane_output) = Self::run_tmux_cmd(&[
                "list-panes",
                "-t",
                session_name,
                "-F",
                "#{pane_id}",
            ]).await {
                let pane_id = String::from_utf8_lossy(&pane_output.stdout)
                    .lines()
                    .next()
                    .map(|l| l.trim().to_string())
                    .unwrap_or_default();
                if !pane_id.is_empty() {
                    metadata.insert("default_pane_id".to_string(), pane_id);
                }
            }
            workspaces.push(WorkspaceHandle {
                id,
                backend: "tmux".to_string(),
                metadata,
            });
        }

        Ok(workspaces)
    }

    async fn cleanup_workspace(&self, handle: &WorkspaceHandle) -> ErgataiResult<()> {
        let session = Self::session_name_from_handle(handle)?;
        info!(session = session, "Cleaning up tmux session");

        if let Err(e) =
            Self::run_tmux_cmd_checked(&["kill-session", "-t", &session], "Failed to kill session")
                .await
        {
            warn!(session = session, error = %e, "kill-session failed (may already be gone)");
        }

        Ok(())
    }

    async fn shutdown(&self) -> ErgataiResult<()> {
        let workspaces = self.list_workspaces().await?;
        for ws in &workspaces {
            self.cleanup_workspace(ws).await?;
        }
        info!(
            count = workspaces.len(),
            "Shutdown: cleaned up all workspaces"
        );
        Ok(())
    }

    async fn discover_agents(&self) -> ErgataiResult<Vec<(String, AgentHandle)>> {
        // List all tmux sessions
        let sessions_output =
            Self::run_tmux_cmd(&["list-sessions", "-F", "#{session_name}"]).await?;
        if !sessions_output.status.success() {
            debug!("No tmux sessions found (or tmux not running)");
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&sessions_output.stdout);

        // Scan ALL sessions — not just prefix-matched ones.
        // The session_prefix is for workspace management (creating/cleaning).
        // For discovery, we need to find agents in any session.
        let all_sessions: Vec<String> = stdout.lines().map(|l| l.to_string()).collect();

        if all_sessions.is_empty() {
            debug!("No tmux sessions found");
            return Ok(Vec::new());
        }

        // Delimiter for pane format parsing
        let format = "#{pane_id}|#{pane_current_command}|#{pane_pid}".to_string();

        let mut discovered = Vec::new();

        for session_name in &all_sessions {
            let output = match Self::run_tmux_cmd(&[
                "list-panes",
                "-t",
                session_name,
                "-F",
                &format,
            ])
            .await
            {
                Ok(o) => o,
                Err(e) => {
                    warn!(session = session_name, error = %e, "Failed to list panes");
                    continue;
                }
            };

            if !output.status.success() {
                continue;
            }

            let pane_stdout = String::from_utf8_lossy(&output.stdout);
            for line in pane_stdout.lines() {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() < 3 {
                    debug!("Skipping malformed pane line: {:?}", line);
                    continue;
                }

                let pane_id = parts[0];
                let command = parts[1];
                let pid_str = parts[2];

                let pid: u32 = pid_str.parse().unwrap_or(0);
                let agent_id = read_tmux_pane_env(pid)
                    .unwrap_or_else(|| format!("pane_{}", pane_id.replace('%', "")));

                // Build a synthetic WorkspaceHandle for this session.
                // Strip the session prefix to recover the original workspace_id,
                // so it matches what create_workspace stored (spec.id).
                let workspace_id = session_name
                    .strip_prefix(&format!("{}-", self.session_prefix))
                    .unwrap_or(session_name)
                    .to_string();
                let mut ws_metadata = HashMap::new();
                ws_metadata.insert("session".to_string(), session_name.clone());
                ws_metadata.insert("default_pane_id".to_string(), pane_id.to_string());
                ws_metadata.insert("persist".to_string(), "false".to_string());
                let workspace = WorkspaceHandle {
                    id: workspace_id.clone(),
                    backend: "tmux".to_string(),
                    metadata: ws_metadata,
                };

                // Build AgentHandle with pane_id + session
                let mut metadata = HashMap::new();
                metadata.insert("pane_id".to_string(), pane_id.to_string());
                metadata.insert("session".to_string(), session_name.clone());
                // Read ERGATAI_AGENT_ID from the pane's child process environment.
                // The startup script sets ERGATAI_AGENT_ID, then exec's opencode.
                // This is the identifier used in MCP URL paths (e.g., /mcp/agent-2).
                // Fall back to workspace_id if ERGATAI_AGENT_ID is not set.
                let ergatai_agent_id = if pid > 0 {
                    read_proc_environ(pid, "ERGATAI_AGENT_ID")
                        .or_else(|| find_child_environ(pid, "ERGATAI_AGENT_ID"))
                        .unwrap_or_else(|| workspace_id.clone())
                } else {
                    workspace_id.clone()
                };
                metadata.insert("ergatai_agent_id".to_string(), ergatai_agent_id);

                let handle = AgentHandle {
                    workspace,
                    agent_id: agent_id.clone(),
                    process_id: if pid > 0 { Some(pid.to_string()) } else { None },
                    metadata,
                };

                info!(
                    agent_id = agent_id,
                    session = session_name,
                    pane_id = pane_id,
                    command = command,
                    "Discovered agent in tmux pane"
                );

                discovered.push((agent_id, handle));
            }
        }

        info!(
            count = discovered.len(),
            sessions = all_sessions.len(),
            "Discovery scan complete"
        );
        Ok(discovered)
    }
}

// ── Helper functions ──

/// Read an environment variable from a process's `/proc/{pid}/environ`.
///
/// Linux-specific: reads the null-delimited environment block from procfs.
/// Returns `None` if the process doesn't exist, permission is denied,
/// or the variable is not set.
fn read_proc_environ(pid: u32, var_name: &str) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let data = std::fs::read(format!("/proc/{}/environ", pid)).ok()?;
    let prefix = format!("{}=", var_name);
    data.split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .find_map(|entry| entry.strip_prefix(&prefix).map(|v| v.to_string()))
}

/// Find an environment variable from a child process (e.g., opencode).
///
/// The startup script (bash) exec's opencode, so we scan
/// /proc/{pid}/task/{pid}/children to find the opencode process and read its env.
fn find_child_environ(pid: u32, var_name: &str) -> Option<String> {
    let children_path = format!("/proc/{}/task/{}/children", pid, pid);
    let children_data = std::fs::read_to_string(&children_path).ok()?;

    for child_pid_str in children_data.split_whitespace() {
        if let Ok(child_pid) = child_pid_str.parse::<u32>() {
            let comm_path = format!("/proc/{}/comm", child_pid);
            if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                let name = comm.trim();
                // Match both "opencode" and "opencode.exe" (the ELF binary name)
                if name == "opencode" || name == "opencode.exe" {
                    return read_proc_environ(child_pid, var_name);
                }
            }
        }
    }
    None
}

/// Read `TMUX_PANE` environment variable from a process's `/proc/{pid}/environ`.
///
/// tmux automatically sets `TMUX_PANE=%N` in every pane's environment.
/// Reading it from `/proc` gives us the deterministic pane identifier
/// (e.g., `%15`) that survives discovery rescans — unlike sequential `pane_N` IDs.
fn read_tmux_pane_env(pid: u32) -> Option<String> {
    read_proc_environ(pid, "TMUX_PANE")
}

/// Sanitize a message for safe tmux injection.
fn sanitize_message(message: &str) -> String {
    let stripped = strip_ansi(message);

    let single_line: String = stripped
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    if single_line.len() > MAX_MESSAGE_SIZE {
        let mut end = MAX_MESSAGE_SIZE;
        while end > 0 && !single_line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... [truncated]", &single_line[..end])
    } else {
        single_line
    }
}

/// Strip ANSI escape sequences (CSI: ESC [ ... final_byte).
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                loop {
                    match chars.peek() {
                        Some(&b) if (0x20..=0x3F).contains(&(b as u32)) => {
                            chars.next();
                        }
                        Some(_) => {
                            chars.next();
                            break;
                        }
                        None => break,
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_simple() {
        assert_eq!(sanitize_message("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_strips_newlines() {
        let result = sanitize_message("line1\nline2\rline3");
        assert_eq!(result, "line1 line2 line3");
    }

    #[test]
    fn test_sanitize_strips_ansi() {
        let result = sanitize_message("\x1b[31mRED\x1b[0m normal");
        assert_eq!(result, "RED normal");
    }

    #[test]
    fn test_sanitize_truncates() {
        let big = "x".repeat(MAX_MESSAGE_SIZE + 100);
        let result = sanitize_message(&big);
        assert!(result.len() <= MAX_MESSAGE_SIZE + 20);
        assert!(result.ends_with("[truncated]"));
    }

    #[test]
    fn test_strip_ansi_colors() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_no_escapes() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn test_session_name() {
        let backend = TmuxBackend::new("ergatai");
        assert_eq!(backend.session_name("task-123"), "ergatai-task-123");
    }

    #[test]
    fn test_session_name_sanitizes() {
        let backend = TmuxBackend::new("ergatai");
        assert_eq!(backend.session_name("a|b:c.d"), "ergatai-a-b-c-d");
    }

    #[test]
    fn test_capabilities() {
        let backend = TmuxBackend::new("test");
        let caps = backend.capabilities();
        assert!(caps.supports_message_injection);
        assert!(caps.supports_output_capture);
        assert!(!caps.supports_resource_limits);
    }
}
