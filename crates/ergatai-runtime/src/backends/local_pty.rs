//! LocalPtyBackend — tmux-based agent execution environment.
//!
//! This backend preserves the current tmux integration: agents run in tmux panes,
//! messages are injected via `send-keys -l`, and output is captured via `capture-pane`.
//!
//! The key improvement over the old `TmuxManager`: state is managed through
//! `WorkspaceHandle` and `AgentHandle` (opaque, backend-specific metadata),
//! so the AgentLauncher lifetime bug is fixed — handles are passed around
//! instead of relying on per-instance HashMaps.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::{debug, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use crate::backend::AgentRuntimeBackend;
use crate::types::{
    AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec,
};

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

// ── LocalPtyBackend ──

/// tmux-based agent execution backend.
///
/// Each workspace is a tmux session. Each agent is a pane within that session.
/// Session names follow the pattern `{prefix}-{workspace_id}`.
pub struct LocalPtyBackend {
    /// Session name prefix (e.g., "ergatai" or "ergatai-opencode")
    session_prefix: String,
    /// Default terminal dimensions
    width: u32,
    height: u32,
}

impl LocalPtyBackend {
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
                debug!("Retrying tmux command (attempt {}): {:?}", attempt + 1, args);
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
}

#[async_trait]
impl AgentRuntimeBackend for LocalPtyBackend {
    fn name(&self) -> &'static str {
        "local-pty"
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

        let result = Self::run_tmux_cmd(&[
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-x",
            &self.width.to_string(),
            "-y",
            &self.height.to_string(),
        ])
        .await;

        if let Err(e) = result {
            let err_str = e.to_string();
            if !err_str.contains("duplicate") && !err_str.contains("already exists") {
                return Err(e);
            }
            debug!("Session {} already exists, reusing", session_name);
        }

        let mut metadata = HashMap::new();
        metadata.insert("session".to_string(), session_name.clone());

        Ok(WorkspaceHandle {
            id: spec.id,
            backend: "local-pty".to_string(),
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
            return Err(ErgataiError::internal("Agent command must not be empty".to_string()));
        }
        if command.contains('\n') || command.contains('\r') {
            return Err(ErgataiError::internal(
                "Agent command must not contain newlines".to_string(),
            ));
        }

        let output = Self::run_tmux_cmd(&[
            "split-window",
            "-t",
            &session,
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
        ])
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ErgataiError::internal(format!(
                "Failed to split window: {}",
                stderr.trim()
            )));
        }

        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        Self::run_tmux_cmd_checked(
            &["send-keys", "-l", "-t", &pane_id, command],
            "Failed to send command to pane",
        )
        .await?;

        Self::run_tmux_cmd_checked(
            &["send-keys", "-t", &pane_id, "Enter"],
            "Failed to send Enter to pane",
        )
        .await?;

        info!(pane_id = pane_id, session = session, "Agent started in tmux pane");

        if let Some(instr) = instruction {
            tokio::time::sleep(INSTRUCTION_DELAY).await;
            Self::send_to_pane(&pane_id, instr).await?;
            info!(pane_id = pane_id, "Instruction injected ({}B)", instr.len());
        }

        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());
        let mut metadata = HashMap::new();
        metadata.insert("pane_id".to_string(), pane_id.clone());

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

        if let Err(e) = Self::run_tmux_cmd_checked(
            &["kill-pane", "-t", &pane_id],
            "Failed to kill pane",
        )
        .await
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
        let start = Instant::now();

        loop {
            match self.is_alive(handle).await {
                Ok(false) => return Ok(WaitResult::Exited { code: 0 }),
                Ok(true) => {}
                Err(e) => return Ok(WaitResult::Error(e.to_string())),
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
        let output =
            Self::run_tmux_cmd(&["list-sessions", "-F", "#{session_name}"]).await?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let prefix = format!("{}-", self.session_prefix);

        let workspaces = stdout
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .map(|session_name| {
                let id = session_name
                    .strip_prefix(&prefix)
                    .unwrap_or(session_name)
                    .to_string();
                let mut metadata = HashMap::new();
                metadata.insert("session".to_string(), session_name.to_string());
                WorkspaceHandle {
                    id,
                    backend: "local-pty".to_string(),
                    metadata,
                }
            })
            .collect();

        Ok(workspaces)
    }

    async fn cleanup_workspace(&self, handle: &WorkspaceHandle) -> ErgataiResult<()> {
        let session = Self::session_name_from_handle(handle)?;
        info!(session = session, "Cleaning up tmux session");

        if let Err(e) = Self::run_tmux_cmd_checked(
            &["kill-session", "-t", &session],
            "Failed to kill session",
        )
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
        info!(count = workspaces.len(), "Shutdown: cleaned up all workspaces");
        Ok(())
    }
}

// ── Helper functions (ported from tmux.rs) ──

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
        let backend = LocalPtyBackend::new("ergatai");
        assert_eq!(backend.session_name("task-123"), "ergatai-task-123");
    }

    #[test]
    fn test_session_name_sanitizes() {
        let backend = LocalPtyBackend::new("ergatai");
        assert_eq!(backend.session_name("a|b:c.d"), "ergatai-a-b-c-d");
    }

    #[test]
    fn test_capabilities() {
        let backend = LocalPtyBackend::new("test");
        let caps = backend.capabilities();
        assert!(caps.supports_message_injection);
        assert!(caps.supports_output_capture);
        assert!(!caps.supports_resource_limits);
    }
}
