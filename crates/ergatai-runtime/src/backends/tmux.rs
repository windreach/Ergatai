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
use std::path::PathBuf;
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

// ── tmux binary resolution ──

/// Get the tmux binary path, using the shared cache from `ergatai_binary`.
///
/// Delegates to [`ergatai_binary::find_tmux_binary_cached`] which resolves
/// the path once per process (env var → bundled → sibling → PATH) and caches
/// the result in a global `OnceLock`.
fn tmux_binary() -> ErgataiResult<&'static PathBuf> {
    ergatai_binary::find_tmux_binary_cached().map_err(|e| {
        ErgataiError::internal(format!("tmux binary not found: {}", e))
    })
}

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
    ///
    /// Resolves the tmux binary path once via [`ergatai_binary::find_tmux_binary`]
    /// (env var → bundled → sibling → PATH) and caches it. Subsequent calls
    /// reuse the cached path without re-searching.
    async fn run_tmux_cmd(args: &[&str]) -> ErgataiResult<std::process::Output> {
        let tmux_path = tmux_binary()?;
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
                tokio::process::Command::new(tmux_path).args(args).output(),
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

    /// Best-effort exit code retrieval from a dead pane via tmux format variables.
    ///
    /// Uses `display-message -p '#{pane_exit_status}'` to query the pane's exit code.
    /// Requires tmux >= 3.3 (which introduced `pane_exit_status`). Returns 0 on
    /// older versions or if the pane is already cleaned up.
    async fn read_exit_code_from_tmux(&self, pane_id: &str) -> i32 {
        // Try reading the status file first (may have been written by a prior hook)
        let sanitized = pane_id.replace('%', "-");
        let status_file = format!("/tmp/ergatai-exit-{}.status", sanitized);
        if let Some(code) = Self::read_exit_code_file(&status_file).await {
            let _ = tokio::fs::remove_file(&status_file).await;
            return code;
        }

        // Query tmux directly — works briefly after pane death before cleanup
        match Self::run_tmux_cmd(&[
            "display-message",
            "-t",
            pane_id,
            "-p",
            "#{pane_exit_status}",
        ])
        .await
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.trim().parse::<i32>().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Read exit code from the status file written by the pane-died hook.
    ///
    /// Includes a small retry loop to handle the race between `wait-for` returning
    /// and the filesystem flush completing in the hook's `run-shell`.
    async fn read_exit_code_file(path: &str) -> Option<i32> {
        for _ in 0..10 {
            if let Ok(contents) = tokio::fs::read_to_string(path).await {
                let trimmed = contents.trim();
                if !trimmed.is_empty() {
                    if let Ok(code) = trimmed.parse::<i32>() {
                        return Some(code);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
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
                Ok(false) => return Ok(exit_code_to_wait_result(0)),
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

    // ── Multi-agent workspace helpers ──

    /// Count the number of panes in a tmux session.
    ///
    /// Returns 0 if the session doesn't exist or the query fails.
    /// Used by `start_agent` to determine whether this is the first agent
    /// (1 pane = default, use it) or a subsequent agent (2+ panes, split new).
    async fn count_panes_in_session(session: &str) -> usize {
        match Self::run_tmux_cmd(&[
            "list-panes", "-t", session, "-F", "#{pane_id}",
        ])
        .await
        {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).lines().count()
            }
            _ => 0,
        }
    }

    /// Find a pane in the given session that has a running foreground process.
    ///
    /// Used for workspace reuse: when reconnecting to a pre-existing session,
    /// we want to reattach to a pane that already has an agent running rather
    /// than spawning into the default (possibly idle shell) pane.
    ///
    /// Returns the pane_id of the first running pane found, or `None` if all
    /// panes are dead or the session doesn't exist.
    async fn find_running_pane(session: &str) -> ErgataiResult<Option<String>> {
        let output = match Self::run_tmux_cmd(&[
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_id}|#{pane_current_command}|#{pane_pid}",
        ])
        .await
        {
            Ok(o) if o.status.success() => o,
            _ => return Ok(None),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }
            let pane_id = parts[0].trim();
            let command = parts[1].trim();
            let pid: u32 = parts[2].trim().parse().unwrap_or(0);

            // Skip panes running just a shell (idle, no agent).
            // A pane running bash/sh with no child is an idle shell.
            if pid > 0 && !matches!(command, "bash" | "sh" | "zsh" | "fish" | "dash") {
                return Ok(Some(pane_id.to_string()));
            }
            // Also check if the shell has a child process (agent running inside it).
            if pid > 0 {
                #[cfg(target_os = "linux")]
                {
                    use super::proc_linux::read_proc_state;
                    // Check if there's a child process in Running/Sleeping state.
                    if let Some(child_pid) = super::proc_linux::find_child_pid(pid) {
                        if let Ok(state) = read_proc_state(child_pid) {
                            if matches!(
                                state,
                                super::proc_linux::ProcessState::Running
                                    | super::proc_linux::ProcessState::Sleeping
                            ) {
                                return Ok(Some(pane_id.to_string()));
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    // ── Graceful stop ──

    /// Gracefully stop a pane: send an exit command, wait for grace period, then force-kill.
    ///
    /// `exit_command` can be:
    /// - A key name like `"C-c"` (Ctrl-C) or `"C-\\"` (Ctrl-Backslash / SIGQUIT)
    /// - A text string like `"exit\n"` or `"/exit"` (sent as send-keys text + Enter)
    ///
    /// After sending the exit command, waits up to `grace_period` for the pane to die.
    /// If the pane is still alive after the grace period, force-kills via `kill-pane`.
    pub async fn graceful_stop_pane(
        _session: &str,
        pane_id: &str,
        exit_command: &str,
        grace_period: Duration,
    ) -> ErgataiResult<()> {
        // Determine if exit_command is a tmux key name or text to type.
        // Uses exact matching to avoid false positives (e.g., "Create-file" should
        // NOT be treated as key "C-" + "reate-file").
        let is_key = is_tmux_key_name(exit_command);

        if is_key {
            Self::run_tmux_cmd(&["send-keys", "-t", pane_id, exit_command]).await?;
        } else {
            // Send as text + Enter.
            let sanitized = sanitize_message(exit_command);
            Self::run_tmux_cmd(&["send-keys", "-l", "-t", pane_id, &sanitized]).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            Self::run_tmux_cmd(&["send-keys", "-t", pane_id, "Enter"]).await?;
        }

        // Wait for the pane to die within the grace period.
        let poll_interval = Duration::from_millis(200);
        let deadline = Instant::now() + grace_period;
        while Instant::now() < deadline {
            // Check if pane is still alive.
            let alive = Self::run_tmux_cmd(&["list-panes", "-t", pane_id])
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !alive {
                debug!(pane_id = pane_id, "Pane exited gracefully");
                return Ok(());
            }
            tokio::time::sleep(poll_interval).await;
        }

        // Grace period expired — force kill.
        warn!(
            pane_id = pane_id,
            grace_period = ?grace_period,
            "Pane did not exit gracefully, force-killing"
        );
        let _ = Self::run_tmux_cmd(&["kill-pane", "-t", pane_id]).await;
        Ok(())
    }

    /// Gracefully stop an agent using its handle.
    ///
    /// Convenience wrapper around [`graceful_stop_pane`] that extracts pane_id from the handle.
    pub async fn graceful_stop_agent(
        &self,
        handle: &AgentHandle,
        exit_command: &str,
        grace_period: Duration,
    ) -> ErgataiResult<()> {
        let pane_id = Self::pane_id(handle)?;
        let session = Self::session_name_from_handle(&handle.workspace)?;
        Self::graceful_stop_pane(&session, &pane_id, exit_command, grace_period).await
    }

    // ── Smart completion detection ──

    /// Poll a pane's visible terminal for specific text.
    ///
    /// Captures the pane content repeatedly until `text` is found or `timeout` expires.
    /// Returns `true` if the text was found, `false` on timeout.
    pub async fn wait_for_text(
        session: &str,
        pane_id: &str,
        text: &str,
        timeout: Duration,
    ) -> ErgataiResult<bool> {
        let poll_interval = Duration::from_millis(500);
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            if let Ok(Some(captured)) = Self::capture_pane_by_id(session, pane_id).await {
                if captured.contains(text) {
                    return Ok(true);
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
        Ok(false)
    }

    /// Race multiple completion patterns against a pane's visible terminal.
    ///
    /// Returns the first matching pattern, or `None` if the timeout expires
    /// without any match. Useful for detecting agent readiness:
    ///
    /// ```ignore
    /// let ready = backend.expect_completion(
    ///     "ergatai-task1", "%5",
    ///     &["How can I help you", "$ ", "Ready"],
    ///     Duration::from_secs(30),
    /// ).await?;
    /// ```
    pub async fn expect_completion(
        session: &str,
        pane_id: &str,
        patterns: &[&str],
        timeout: Duration,
    ) -> ErgataiResult<Option<String>> {
        let poll_interval = Duration::from_millis(500);
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            if let Ok(Some(captured)) = Self::capture_pane_by_id(session, pane_id).await {
                for pattern in patterns {
                    if captured.contains(pattern) {
                        return Ok(Some(pattern.to_string()));
                    }
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
        Ok(None)
    }

    // ── Daemon-driven pane operations (by session + pane_id) ──
    // These work across Ergatai restarts since they don't rely on local handles.

    /// Inject a message into any pane identified by session name and pane_id.
    ///
    /// Unlike [`inject_message`](AgentRuntimeBackend::inject_message), this does not
    /// require an `AgentHandle` — it works with just the tmux identifiers. This is
    /// useful for re-injecting into agents discovered via `discover_agents()`.
    pub async fn inject_message_to_pane(
        _session: &str,
        pane_id: &str,
        message: &str,
    ) -> ErgataiResult<()> {
        Self::send_to_pane(pane_id, message).await
    }

    /// Stop (close) a pane by session name and pane_id.
    pub async fn stop_pane_by_id(
        _session: &str,
        pane_id: &str,
    ) -> ErgataiResult<()> {
        let _ = Self::run_tmux_cmd(&["kill-pane", "-t", pane_id]).await;
        Ok(())
    }

    /// Force-kill a pane (alias for [`stop_pane_by_id`]).
    pub async fn kill_pane_by_id(
        session: &str,
        pane_id: &str,
    ) -> ErgataiResult<()> {
        Self::stop_pane_by_id(session, pane_id).await
    }

    /// Capture pane output by session name and pane_id.
    pub async fn capture_pane_by_id(
        _session: &str,
        pane_id: &str,
    ) -> ErgataiResult<Option<String>> {
        let output = Self::run_tmux_cmd(&["capture-pane", "-t", pane_id, "-p"]).await?;
        if !output.status.success() {
            return Ok(None);
        }
        let raw = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(Some(strip_ansi(&raw)))
    }

    // ── Enhanced diagnostics ──

    /// Get detailed per-pane information for all panes in managed sessions.
    ///
    /// Returns a vector of [`TmuxPaneInfo`] structs with PID, command, cwd,
    /// and other details. Used by `/api/v1/status` and health monitoring.
    pub async fn tmux_status_detailed(&self) -> Vec<TmuxPaneInfo> {
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

        let mut panes = Vec::new();

        for session in &sessions {
            let pane_output = match Self::run_tmux_cmd(&[
                "list-panes",
                "-t",
                session,
                "-F",
                "#{pane_id}|#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{pane_width}|#{pane_height}",
            ])
            .await
            {
                Ok(o) if o.status.success() => o,
                _ => continue,
            };

            let pane_stdout = String::from_utf8_lossy(&pane_output.stdout);
            for line in pane_stdout.lines() {
                let parts: Vec<&str> = line.splitn(6, '|').collect();
                if parts.len() < 6 {
                    continue;
                }
                panes.push(TmuxPaneInfo {
                    session: session.clone(),
                    pane_id: parts[0].to_string(),
                    pid: parts[1].parse().unwrap_or(0),
                    command: parts[2].to_string(),
                    cwd: parts[3].to_string(),
                    width: parts[4].parse().unwrap_or(0),
                    height: parts[5].parse().unwrap_or(0),
                });
            }
        }

        panes
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

/// Detailed information about a single tmux pane.
///
/// Returned by [`TmuxBackend::tmux_status_detailed`] for enhanced diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct TmuxPaneInfo {
    /// tmux session name this pane belongs to
    pub session: String,
    /// tmux pane ID (e.g., "%5")
    pub pane_id: String,
    /// PID of the pane's foreground process
    pub pid: u32,
    /// Currently running command (e.g., "opencode", "bash")
    pub command: String,
    /// Current working directory of the pane
    pub cwd: String,
    /// Pane width in columns
    pub width: u32,
    /// Pane height in rows
    pub height: u32,
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
        let tmux_path = tmux_binary()?;
        info!(path = %tmux_path.display(), "tmux binary resolved");
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
        let work_dir = spec.work_dir.to_string_lossy().to_string();

        info!(
            session = session_name,
            width = self.width,
            height = self.height,
            cwd = work_dir,
            "Creating tmux session workspace"
        );

        // Build new-session args with optional -c for working directory.
        // Bind `.to_string()` results to locals so they outlive the `args` borrow.
        let width_str = self.width.to_string();
        let height_str = self.height.to_string();
        let args = vec![
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-x",
            &width_str,
            "-y",
            &height_str,
            "-c",
            &work_dir,
            "-P",
            "-F",
            "#{pane_id}",
        ];

        // Capture pane_id from new-session output
        let result = Self::run_tmux_cmd(&args).await;

        let mut metadata = HashMap::new();
        metadata.insert("session".to_string(), session_name.clone());
        // Track that this workspace was freshly created (for reuse detection).
        metadata.insert("fresh".to_string(), "true".to_string());
        metadata.insert("work_dir".to_string(), work_dir.clone());

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
                // For reused sessions, this is NOT a fresh workspace.
                metadata.insert("fresh".to_string(), "false".to_string());
                // Find the first pane (anchor for future splits) and any running panes.
                if let Ok(output) = Self::run_tmux_cmd(&[
                    "list-panes",
                    "-t",
                    &session_name,
                    "-F",
                    "#{pane_id}|#{pane_current_command}|#{pane_pid}",
                ]).await {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let mut first_pane = None;
                    for line in stdout.lines() {
                        let parts: Vec<&str> = line.splitn(3, '|').collect();
                        if parts.len() < 3 { continue; }
                        if first_pane.is_none() {
                            first_pane = Some(parts[0].trim().to_string());
                        }
                    }
                    if let Some(pane_id) = first_pane {
                        metadata.insert("default_pane_id".to_string(), pane_id.clone());
                        // Set as anchor pane for future splits.
                        metadata.insert("anchor_pane".to_string(), pane_id);
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

        let is_fresh = handle.metadata.get("fresh").map(|v| v == "true").unwrap_or(true);
        let persist = handle.metadata.get("persist")
            .map(|v| v == "true")
            .unwrap_or(false);

        // Determine which pane to use for this agent.
        // Query tmux to count existing panes — more reliable than metadata flags
        // which may not propagate correctly across multiple start_agent calls.
        let existing_pane_count = Self::count_panes_in_session(&session).await;
        let is_first_agent = existing_pane_count <= 1;

        let (pane_id, is_first_agent) = if !is_fresh && is_first_agent {
            // ── Workspace reuse: first agent in a pre-existing session ──
            // Find an existing running pane to reattach to.
            match Self::find_running_pane(&session).await? {
                Some(existing_pane) => {
                    debug!(pane_id = %existing_pane, "Reattaching to existing pane in reused workspace");
                    (existing_pane, true)
                }
                None => {
                    // No running pane — use the default pane from new-session/list-panes.
                    let default_pane = handle.metadata.get("default_pane_id").cloned()
                        .ok_or_else(|| ErgataiError::internal("Missing default_pane_id in reused workspace"))?;
                    (default_pane, true)
                }
            }
        } else if !is_first_agent {
            // ── Multi-agent: 2nd+ agent — split from anchor pane ──
            // Use the first pane in the session as the split target.
            let anchor = handle.metadata.get("anchor_pane").cloned()
                .or_else(|| handle.metadata.get("default_pane_id").cloned())
                .ok_or_else(|| ErgataiError::internal("Missing anchor pane for multi-agent split"))?;
            let mut split_args = vec![
                "split-window", "-h",
                "-t", &anchor,
                "-P", "-F", "#{pane_id}",
            ];
            // Apply cwd from workspace metadata.
            if let Some(dir) = handle.metadata.get("work_dir") {
                split_args.push("-c");
                split_args.push(dir);
            }
            let output = Self::run_tmux_cmd(&split_args).await?;
            let new_pane = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if new_pane.is_empty() {
                return Err(ErgataiError::internal(
                    "split-window returned empty pane_id".to_string(),
                ));
            }
            debug!(
                anchor = %anchor,
                new_pane = %new_pane,
                "Split new pane from anchor for multi-agent"
            );
            (new_pane, false)
        } else {
            // ── Fresh workspace: first agent uses the default pane ──
            let default_pane = handle.metadata.get("default_pane_id").cloned()
                .ok_or_else(|| ErgataiError::internal("Missing default_pane_id in workspace metadata"))?;
            (default_pane, true)
        };

        // Non-persist: exec replaces shell with agent process
        //   → agent exits → pane dies → pane-died hook → pane cleanup
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

        // Set pane title (requires tmux >= 3.2 for -T flag on select-pane).
        // Best-effort: older tmux versions ignore the flag silently.
        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());
        let title = handle.metadata.get("title")
            .cloned()
            .unwrap_or_else(|| agent_id.clone());
        let _ = Self::run_tmux_cmd(&[
            "select-pane", "-t", &pane_id, "-T", &title,
        ]).await;

        info!(
            pane_id = pane_id,
            session = session,
            title = title,
            is_first = is_first_agent,
            "Agent started in tmux pane"
        );

        if let Some(instr) = instruction {
            tokio::time::sleep(INSTRUCTION_DELAY).await;
            Self::send_to_pane(&pane_id, instr).await?;
            info!(pane_id = pane_id, "Instruction injected ({}B)", instr.len());
        }

        // Set auto-kill hooks only for non-persist, first-agent (single-agent) sessions.
        // Multi-agent sessions: don't kill the whole session on one pane's death or detach.
        // Note: if a second agent is later added to a session that already has these hooks,
        // the hooks will kill the whole session on first agent exit — callers should use
        // persist=true for multi-agent workloads.
        if !persist && is_first_agent {
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

        // For multi-agent: mark this workspace as having an anchor pane
        // so subsequent start_agent calls will split instead of reusing default.
        // The anchor is stored in the workspace handle's metadata (caller should
        // update it). We also store it in the agent metadata for discoverability.
        let mut metadata = HashMap::new();
        metadata.insert("pane_id".to_string(), pane_id.clone());
        metadata.insert("session".to_string(), session.clone());
        metadata.insert("title".to_string(), title);
        // Store workspace ID as ergatai_agent_id initially.
        // discover_agents() will later read ERGATAI_AGENT_ID from the child
        // process and update this to the correct MCP binding identifier.
        metadata.insert("ergatai_agent_id".to_string(), handle.id.clone());
        if is_first_agent {
            // First agent becomes the anchor for future splits.
            metadata.insert("anchor_pane".to_string(), pane_id.clone());
        }

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
        let session = Self::session_name_from_handle(&handle.workspace)?;
        info!(pane_id = pane_id, "Stopping agent (graceful: C-c then kill)");

        // Graceful: send Ctrl-C, wait up to 2s for pane to exit, then force-kill.
        Self::graceful_stop_pane(&session, &pane_id, "C-c", Duration::from_secs(2)).await
    }

    async fn kill_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        let pane_id = Self::pane_id(handle)?;
        info!(pane_id = pane_id, "Force-killing agent pane");

        // Force: immediate kill-pane, no grace period.
        if let Err(e) =
            Self::run_tmux_cmd_checked(&["kill-pane", "-t", &pane_id], "Failed to kill pane").await
        {
            warn!(pane_id = pane_id, error = %e, "Failed to kill pane (may already be closed)");
        }

        Ok(())
    }

    async fn wait_for_exit(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult> {
        let pane_id = Self::pane_id(handle)?;
        let session = Self::session_name_from_handle(&handle.workspace)?;
        let sanitized = pane_id.replace('%', "-");
        let channel = format!("ergatai-exit-{}", sanitized);
        let status_file = format!("/tmp/ergatai-exit-{}.status", sanitized);
        // Resolve tmux binary path for use inside the shell hook.
        // The hook runs via tmux's `run-shell`, which invokes /bin/sh, so we
        // embed the absolute path rather than relying on $PATH inside the shell.
        let tmux_bin = tmux_binary()?.to_string_lossy();

        // 1. Install per-session pane-died hook that:
        //    - Captures exit status via tmux `pane_exit_status` format variable
        //    - Writes it to a temp file for retrieval after wait-for returns
        //    - Signals the per-pane wait-for channel
        //    Using `-t <session>` instead of `-g` avoids clobbering global hooks.
        //    set-hook is idempotent — re-setting replaces the previous hook.
        //    Note: `pane_exit_status` requires tmux >= 3.3. On older versions
        //    the variable expands to empty and we fall back to exit code 0.
        let hook_cmd = format!(
            "run-shell \"\
                code=0; \
                if c=$({tmux_bin} display-message -t '{pane_id}' -p '#{{pane_exit_status}}' 2>/dev/null) && [ -n \\\"$c\\\" ]; then \
                    code=$c; \
                fi; \
                echo \\\"$code\\\" > {status_file}; \
                {tmux_bin} wait-for -S {channel} 2>/dev/null || true\
            \""
        );
        Self::run_tmux_cmd(&[
            "set-hook",
            "-t",
            &session,
            "pane-died",
            &hook_cmd,
        ])
        .await?;

        // 2. Check if already dead (handles race: pane died before hook was set)
        match self.is_alive(handle).await {
            Ok(false) => {
                // Pane already dead — try to read exit code from tmux directly.
                // The hook may not have run yet, so we query tmux format directly.
                let code = self.read_exit_code_from_tmux(&pane_id).await;
                return Ok(exit_code_to_wait_result(code));
            }
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
            Ok(Ok(_)) => {
                // Read exit code from the status file written by the hook.
                // Small retry loop handles the race between wait-for returning
                // and the filesystem flush completing.
                let code = Self::read_exit_code_file(&status_file).await.unwrap_or(0);
                // Clean up temp file (best-effort)
                let _ = tokio::fs::remove_file(&status_file).await;
                Ok(exit_code_to_wait_result(code))
            }
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
                    super::proc_linux::read_proc_environ(pid, "ERGATAI_AGENT_ID")
                        .or_else(|| super::proc_linux::find_child_environ(pid, "ERGATAI_AGENT_ID"))
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

/// Check if a string is a valid tmux key name (for `send-keys` without `-l`).
///
/// Matches:
/// - Modifier + single char: `C-a`, `M-x`, `S-b` (exactly 3 chars)
/// - Named keys: `Enter`, `Escape`, `Tab`, `BSpace`, `Up`, `Down`, etc.
/// - Modifier + named key: `C-Up`, `M-Enter`, `S-Tab`, etc.
///
/// Does NOT match arbitrary text that happens to start with `C-` or `M-`
/// (e.g., `"Create-file"` is text, not a key).
fn is_tmux_key_name(s: &str) -> bool {
    // Named keys recognized by tmux send-keys.
    const NAMED_KEYS: &[&str] = &[
        "Enter", "Return", "Escape", "Esc", "Tab", "BSpace", "BackSpace", "Bspace",
        "Up", "Down", "Left", "Right", "Home", "End",
        "NPage", "PPage", "PageUp", "PageDown", "Space", "BTab",
        "Insert", "Delete",
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];

    // Modifier + single char: C-a, M-x, S-b (exactly 3 chars).
    if s.len() == 3 && (s.starts_with("C-") || s.starts_with("M-") || s.starts_with("S-")) {
        return true;
    }

    // Bare named key.
    if NAMED_KEYS.contains(&s) {
        return true;
    }

    // Modifier + named key: C-Up, M-Enter, S-Tab, etc.
    for prefix in &["C-", "M-", "S-"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if NAMED_KEYS.contains(&rest) {
                return true;
            }
        }
    }

    false
}

/// Convert a shell exit code to a `WaitResult`.
///
/// Shell convention: if a process was killed by signal N, the exit code is 128 + N.
/// This allows us to distinguish normal exits from signal-induced deaths and
/// return `WaitResult::Signaled` when appropriate.
fn exit_code_to_wait_result(code: i32) -> WaitResult {
    if code > 128 {
        WaitResult::Signaled {
            signal: code - 128,
        }
    } else {
        WaitResult::Exited { code }
    }
}

/// Read `TMUX_PANE` environment variable from a process's `/proc/{pid}/environ`.
///
/// tmux automatically sets `TMUX_PANE=%N` in every pane's environment.
/// Reading it from `/proc` gives us the deterministic pane identifier
/// (e.g., `%15`) that survives discovery rescans — unlike sequential `pane_N` IDs.
fn read_tmux_pane_env(pid: u32) -> Option<String> {
    super::proc_linux::read_proc_environ(pid, "TMUX_PANE")
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

    #[test]
    fn test_is_tmux_key_name_modifier_single_char() {
        assert!(is_tmux_key_name("C-c"));
        assert!(is_tmux_key_name("C-a"));
        assert!(is_tmux_key_name("M-x"));
        assert!(is_tmux_key_name("S-b"));
    }

    #[test]
    fn test_is_tmux_key_name_named_keys() {
        assert!(is_tmux_key_name("Enter"));
        assert!(is_tmux_key_name("Escape"));
        assert!(is_tmux_key_name("Tab"));
        assert!(is_tmux_key_name("BSpace"));
        assert!(is_tmux_key_name("Up"));
        assert!(is_tmux_key_name("F1"));
        assert!(is_tmux_key_name("F12"));
    }

    #[test]
    fn test_is_tmux_key_name_modifier_plus_named() {
        assert!(is_tmux_key_name("C-Up"));
        assert!(is_tmux_key_name("M-Enter"));
        assert!(is_tmux_key_name("S-Tab"));
        assert!(is_tmux_key_name("C-F1"));
    }

    #[test]
    fn test_is_tmux_key_name_rejects_text() {
        // These look like they start with a modifier but are actually text.
        assert!(!is_tmux_key_name("Create-file"));
        assert!(!is_tmux_key_name("Move-ahead"));
        assert!(!is_tmux_key_name("Save-all"));
        assert!(!is_tmux_key_name("exit"));
        assert!(!is_tmux_key_name("/exit"));
        assert!(!is_tmux_key_name(""));
    }

    #[test]
    fn test_is_tmux_key_name_rejects_long_modifier_strings() {
        // Modifier + multi-char non-key should not match.
        assert!(!is_tmux_key_name("C-abc"));
        assert!(!is_tmux_key_name("M-hello"));
    }
}
