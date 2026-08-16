//! Tmux Manager — terminal multiplexer integration for multi-agent collaboration.
//!
//! Manages tmux sessions and panes to implement message injection and pane
//! content capture. This is a core component of Ergatai's multi-agent
//! collaboration pipeline.
//!
//! # Security
//!
//! All data sent to tmux via `send-keys` uses literal mode (`-l`) so that
//! tmux never interprets content as key names. Commands are executed via
//! `tokio::process::Command` (no shell interpolation). Every tmux command
//! runs with a timeout to prevent indefinite hangs.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};

// ── Configuration constants (override via environment variables) ──

/// Default tmux session name. Override with `ERGATAI_TMUX_SESSION`.
/// Matches the API server's default so `TmuxManager::default()` works
/// out-of-the-box with `ergatai-api`.
const DEFAULT_SESSION_NAME: &str = "ergatai-opencode";

/// Maximum message size in bytes for tmux injection.
/// tmux `send-keys` has practical limits; oversized messages are rejected.
const MAX_MESSAGE_SIZE: usize = 64 * 1024; // 64 KiB

/// Timeout for individual tmux commands. Prevents indefinite hangs.
const TMUX_CMD_TIMEOUT: Duration = Duration::from_secs(10);

/// Number of retries for transient tmux command failures.
const TMUX_CMD_RETRIES: u32 = 2;

/// Delay between retries.
const TMUX_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Delimiter for `list-panes` format string. Chosen to never appear in
/// pane IDs, command names, or PIDs.
const PANE_FORMAT_DELIMITER: char = '|';

// ── Helper: read a config value from env or fall back to default ──

#[must_use]
fn env_or_string(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

// ── Helper: validate identifiers ──

/// Validate an agent ID. Must be non-empty, at most 256 chars, and contain
/// only printable ASCII (no control chars, no whitespace).
fn validate_agent_id(id: &str) -> Result<()> {
    if id.is_empty() {
        anyhow::bail!("agent_id must not be empty");
    }
    if id.len() > 256 {
        anyhow::bail!("agent_id too long ({} > 256)", id.len());
    }
    if id.contains(|c: char| c.is_control() || c.is_whitespace()) {
        anyhow::bail!("agent_id contains control/whitespace characters: {:?}", id);
    }
    Ok(())
}

/// Validate a tmux pane ID. Expected format: `%<digits>` (e.g. `%0`, `%12`).
/// We accept `%<digits>` or `<session>:<window>.<pane>` forms.
fn validate_tmux_pane(pane: &str) -> Result<()> {
    if pane.is_empty() {
        anyhow::bail!("tmux pane ID must not be empty");
    }
    if pane.len() > 128 {
        anyhow::bail!("tmux pane ID too long");
    }
    let valid = if let Some(rest) = pane.strip_prefix('%') {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    } else {
        pane.contains(':') && pane.contains('.')
    };
    if !valid {
        anyhow::bail!("invalid tmux pane ID format: {:?}", pane);
    }
    Ok(())
}

/// Sanitize a message for safe tmux injection.
///
/// - Replaces embedded newlines with spaces so that `send-keys -l` does not
///   trigger premature command execution in the target shell.
/// - Strips ANSI escape sequences that could manipulate the terminal.
/// - Truncates to `MAX_MESSAGE_SIZE`.
#[must_use]
fn sanitize_message(message: &str) -> String {
    let stripped = strip_ansi(message);

    // Replace newlines/carriage-returns with spaces to prevent multi-line injection
    let single_line: String = stripped
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    // Truncate (respecting UTF-8 boundaries)
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

/// Strip ANSI escape sequences (CSI: ESC [ ... final_byte) from a string.
#[must_use]
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                              // Skip parameter + intermediate + final bytes
                loop {
                    match chars.peek() {
                        Some(&b) if (0x20..=0x3F).contains(&(b as u32)) => {
                            chars.next();
                        }
                        Some(_) => {
                            chars.next(); // final byte
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

/// Validate a shell command for safety. Rejects commands containing
/// patterns that would cause unexpected behavior when sent via tmux send-keys.
fn validate_command(command: &str) -> Result<()> {
    if command.is_empty() {
        anyhow::bail!("command must not be empty");
    }
    if command.len() > 4096 {
        anyhow::bail!("command too long ({} > 4096)", command.len());
    }
    if command.contains('\n') || command.contains('\r') {
        anyhow::bail!("command contains newline characters");
    }
    Ok(())
}

// ── Async tmux command runner with retry + timeout ──

/// Run a tmux command with timeout and retry logic.
async fn run_tmux_cmd(args: &[&str]) -> Result<std::process::Output> {
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

        let result =
            tokio::time::timeout(TMUX_CMD_TIMEOUT, Command::new("tmux").args(args).output()).await;

        match result {
            Ok(Ok(output)) => return Ok(output),
            Ok(Err(e)) => {
                last_err = Some(anyhow::anyhow!("tmux exec failed: {}", e));
            }
            Err(_) => {
                last_err = Some(anyhow::anyhow!(
                    "tmux command timed out after {:?}: {:?}",
                    TMUX_CMD_TIMEOUT,
                    args
                ));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("tmux command failed")))
}

/// Run a tmux command and check for success, returning stderr on failure.
async fn run_tmux_cmd_checked(args: &[&str], context_msg: &str) -> Result<()> {
    let output = run_tmux_cmd(args)
        .await
        .map_err(|e| e.context(context_msg.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}: {}", context_msg, stderr.trim());
    }
    Ok(())
}

// ── TmuxAgent ──

/// Information about an agent running inside a tmux pane.
#[derive(Debug, Clone)]
pub struct TmuxAgent {
    pub agent_id: String,
    pub session: String,
    pub pane: String,
    pub command: String,
    /// MCP agent ID if this pane has been mapped to an MCP connection.
    pub mapped_to_mcp: Option<String>,
}

// ── TmuxManager ──

/// Manages tmux sessions, panes, and agent lifecycle.
pub struct TmuxManager {
    /// Default session name (configurable via `ERGATAI_TMUX_SESSION`).
    default_session: String,
    /// Agent registry: agent_id -> TmuxAgent.
    agents: Arc<RwLock<HashMap<String, TmuxAgent>>>,
    /// MCP agent_id -> tmux pane ID mapping.
    mcp_to_tmux_map: Arc<RwLock<HashMap<String, String>>>,
    /// Monotonic counter for pane allocation.
    next_pane_index: Arc<RwLock<u32>>,
}

impl TmuxManager {
    /// Create a new `TmuxManager` with an explicit session name.
    pub fn new(session_name: &str) -> Self {
        Self {
            default_session: session_name.to_string(),
            agents: Arc::new(RwLock::new(HashMap::new())),
            mcp_to_tmux_map: Arc::new(RwLock::new(HashMap::new())),
            next_pane_index: Arc::new(RwLock::new(0)),
        }
    }

    /// Check that the `tmux` binary is available and report its version.
    pub async fn check_tmux() -> Result<()> {
        let output = run_tmux_cmd(&["-V"])
            .await
            .context("Failed to execute tmux. Is tmux installed and in PATH?")?;
        if !output.status.success() {
            anyhow::bail!("tmux -V exited with non-zero status");
        }
        let version = String::from_utf8_lossy(&output.stdout);
        info!("tmux version: {}", version.trim());
        Ok(())
    }

    /// Create a detached tmux session with the given dimensions.
    pub async fn create_session(&self, width: u32, height: u32) -> Result<()> {
        info!(
            "Creating tmux session: {} ({}x{})",
            self.default_session, width, height
        );
        run_tmux_cmd_checked(
            &[
                "new-session",
                "-d",
                "-s",
                &self.default_session,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ],
            "Failed to create tmux session",
        )
        .await
    }

    /// Launch an agent in a new tmux pane.
    ///
    /// The command is sent in **literal mode** (`-l`) to prevent tmux from
    /// interpreting content as key names, followed by a separate `Enter`
    /// key press.
    pub async fn launch_agent(&self, agent_id: &str, command: &str) -> Result<String> {
        validate_agent_id(agent_id)?;
        validate_command(command)?;
        info!(
            "Launching agent {} in session {}",
            agent_id, self.default_session
        );

        // Allocate the next pane index
        let pane_index = {
            let mut next_index = self.next_pane_index.write().await;
            let idx = *next_index;
            *next_index += 1;
            idx
        };

        let target = if pane_index == 0 {
            format!("{}:0.0", self.default_session)
        } else {
            let output = run_tmux_cmd(&[
                "split-window",
                "-t",
                &self.default_session,
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .await
            .context("Failed to split window")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to split window: {}", stderr.trim());
            }

            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        // Send the command in literal mode (prevents key-name interpretation)
        run_tmux_cmd_checked(
            &["send-keys", "-l", "-t", &target, command],
            "Failed to send command to pane",
        )
        .await?;

        // Send Enter as a key name (separate call, NOT in literal mode)
        run_tmux_cmd_checked(
            &["send-keys", "-t", &target, "Enter"],
            "Failed to send Enter to pane",
        )
        .await?;

        let agent = TmuxAgent {
            agent_id: agent_id.to_string(),
            session: self.default_session.clone(),
            pane: target.clone(),
            command: command.to_string(),
            mapped_to_mcp: None,
        };
        self.agents
            .write()
            .await
            .insert(agent_id.to_string(), agent);

        info!("Agent {} launched in pane {}", agent_id, target);
        Ok(target)
    }

    /// Inject a message into an agent's tmux pane.
    ///
    /// The message is sanitized (newlines replaced, ANSI stripped, size
    /// limited) before injection. `Enter` is sent as a separate key press.
    pub async fn inject_message(&self, agent_id: &str, message: &str) -> Result<()> {
        validate_agent_id(agent_id)?;

        let sanitized = sanitize_message(message);
        trace!(
            "Injecting sanitized message to agent {} ({}B)",
            agent_id,
            sanitized.len()
        );

        // Snapshot the pane under read-lock, then release before tmux call
        let pane = {
            let agents = self.agents.read().await;
            let agent = agents
                .get(agent_id)
                .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;
            agent.pane.clone()
        };

        self.send_to_pane(&pane, &sanitized).await?;
        info!("Message injected to agent {}", agent_id);
        Ok(())
    }

    /// Capture the visible content of an agent's tmux pane.
    ///
    /// ANSI escape sequences are stripped from the output.
    pub async fn capture_pane(&self, agent_id: &str) -> Result<String> {
        validate_agent_id(agent_id)?;
        debug!("Capturing output from agent {}", agent_id);

        let pane = {
            let agents = self.agents.read().await;
            let agent = agents
                .get(agent_id)
                .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;
            agent.pane.clone()
        };

        let output = run_tmux_cmd(&["capture-pane", "-t", &pane, "-p"])
            .await
            .context("Failed to capture pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to capture pane: {}", stderr.trim());
        }

        let raw = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(strip_ansi(&raw))
    }

    /// List all registered agents.
    pub async fn list_agents(&self) -> Vec<TmuxAgent> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Get information about a specific agent.
    pub async fn get_agent(&self, agent_id: &str) -> Option<TmuxAgent> {
        self.agents.read().await.get(agent_id).cloned()
    }

    /// Stop an agent by killing its tmux pane.
    pub async fn stop_agent(&self, agent_id: &str) -> Result<()> {
        validate_agent_id(agent_id)?;
        info!("Stopping agent {}", agent_id);

        let agent = self
            .agents
            .write()
            .await
            .remove(agent_id)
            .ok_or_else(|| anyhow::anyhow!("Agent {} not found", agent_id))?;

        if let Err(e) =
            run_tmux_cmd_checked(&["kill-pane", "-t", &agent.pane], "Failed to kill pane").await
        {
            warn!(
                "Failed to kill pane {} (may already be closed): {}",
                agent.pane, e
            );
        }

        info!("Agent {} stopped", agent_id);
        Ok(())
    }

    /// Kill the entire tmux session and clear all agent state.
    pub async fn kill_session(&self) -> Result<()> {
        info!("Killing tmux session: {}", self.default_session);

        if let Err(e) = run_tmux_cmd_checked(
            &["kill-session", "-t", &self.default_session],
            "Failed to kill session",
        )
        .await
        {
            warn!("kill-session failed (may already be gone): {}", e);
        }

        self.agents.write().await.clear();
        self.mcp_to_tmux_map.write().await.clear();
        *self.next_pane_index.write().await = 0;

        info!("Tmux session killed and state cleared");
        Ok(())
    }

    /// Scan tmux session for existing panes and register them as agents.
    ///
    /// Uses `|` as the format delimiter to avoid conflicts with colons that
    /// may appear in command paths. Panes already registered are skipped.
    pub async fn scan_and_register_panes(&self) -> Result<Vec<String>> {
        info!(
            "Scanning tmux session for existing panes: {}",
            self.default_session
        );

        let format = format!(
            "#{{pane_id}}{}#{{pane_current_command}}{}#{{pane_pid}}",
            PANE_FORMAT_DELIMITER, PANE_FORMAT_DELIMITER
        );

        let output = run_tmux_cmd(&["list-panes", "-t", &self.default_session, "-F", &format])
            .await
            .context("Failed to list panes")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to list panes: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut registered = Vec::new();

        // Hold the write lock for the entire scan-and-insert to prevent
        // concurrent registrations from creating duplicates.
        let mut agents = self.agents.write().await;

        for line in stdout.lines() {
            // Use splitn(3, '|') so commands containing '|' are not split further
            let parts: Vec<&str> = line.splitn(3, PANE_FORMAT_DELIMITER).collect();
            if parts.len() < 3 {
                debug!("Skipping malformed pane line: {:?}", line);
                continue;
            }

            let pane_id = parts[0];
            let command = parts[1];
            let pid = parts[2];

            let agent_id = format!("pane_{}", pane_id.replace('%', ""));

            if agents.contains_key(&agent_id) {
                continue;
            }

            let agent = TmuxAgent {
                agent_id: agent_id.clone(),
                session: self.default_session.clone(),
                pane: pane_id.to_string(),
                command: command.to_string(),
                mapped_to_mcp: None,
            };

            agents.insert(agent_id.clone(), agent);
            registered.push(agent_id.clone());

            info!(
                "Registered tmux pane as agent: {} (cmd: {}, pid: {})",
                agent_id, command, pid
            );
        }

        info!("Scanned and registered {} new agents", registered.len());
        Ok(registered)
    }

    /// Check if an agent is registered in this manager.
    pub async fn is_agent_in_tmux(&self, agent_id: &str) -> bool {
        self.agents.read().await.contains_key(agent_id)
    }

    /// Atomically find an unmapped tmux pane and claim it for an MCP agent.
    ///
    /// Lock ordering: `agents` write lock is held across the find-and-claim,
    /// then released before acquiring `mcp_to_tmux_map`. Between the two
    /// locks, no other task can observe a partially-claimed pane because the
    /// `agents` map already reflects the mapping.
    ///
    /// Returns the claimed pane ID, or `None` if no unmapped pane exists.
    pub async fn try_claim_unmapped_pane(&self, mcp_agent_id: &str) -> Option<String> {
        let pane = {
            let mut agents = self.agents.write().await;
            let unmapped = agents.values_mut().find(|a| a.mapped_to_mcp.is_none())?;
            unmapped.mapped_to_mcp = Some(mcp_agent_id.to_string());
            unmapped.pane.clone()
        };

        self.mcp_to_tmux_map
            .write()
            .await
            .insert(mcp_agent_id.to_string(), pane.clone());

        info!("Claimed tmux pane {} for MCP agent {}", pane, mcp_agent_id);
        Some(pane)
    }

    /// Register an explicit MCP-to-tmux pane mapping.
    ///
    /// Validates both `mcp_agent_id` and `tmux_pane` before recording.
    pub async fn register_mcp_to_tmux_mapping(
        &self,
        mcp_agent_id: &str,
        tmux_pane: &str,
    ) -> Result<()> {
        validate_agent_id(mcp_agent_id)?;
        validate_tmux_pane(tmux_pane)?;

        info!(
            "Registering MCP->tmux mapping: {} -> {}",
            mcp_agent_id, tmux_pane
        );

        {
            let mut agents = self.agents.write().await;
            for agent in agents.values_mut() {
                if agent.pane == tmux_pane {
                    agent.mapped_to_mcp = Some(mcp_agent_id.to_string());
                    break;
                }
            }
        }

        self.mcp_to_tmux_map
            .write()
            .await
            .insert(mcp_agent_id.to_string(), tmux_pane.to_string());

        Ok(())
    }

    /// Look up the tmux pane for an MCP agent.
    pub async fn get_tmux_pane_for_mcp_agent(&self, mcp_agent_id: &str) -> Option<String> {
        self.mcp_to_tmux_map.read().await.get(mcp_agent_id).cloned()
    }

    /// Inject a message into an MCP agent's tmux pane via the mapping.
    pub async fn inject_message_by_mcp_id(&self, mcp_agent_id: &str, message: &str) -> Result<()> {
        let tmux_pane = self
            .get_tmux_pane_for_mcp_agent(mcp_agent_id)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No tmux pane mapped for MCP agent {}. \
                     Call register_mcp_to_tmux_mapping first.",
                    mcp_agent_id
                )
            })?;

        let sanitized = sanitize_message(message);
        info!(
            "Injecting message to MCP agent {} via tmux pane {} ({}B)",
            mcp_agent_id,
            tmux_pane,
            sanitized.len()
        );

        self.send_to_pane(&tmux_pane, &sanitized).await?;
        info!("Message injected to MCP agent {} via tmux", mcp_agent_id);
        Ok(())
    }

    // ── Internal helpers ──

    /// Send literal text + Enter to a tmux pane. Shared by
    /// `inject_message` and `inject_message_by_mcp_id`.
    async fn send_to_pane(&self, pane: &str, text: &str) -> Result<()> {
        run_tmux_cmd_checked(
            &["send-keys", "-l", "-t", pane, text],
            "Failed to inject text via tmux",
        )
        .await?;

        run_tmux_cmd_checked(
            &["send-keys", "-t", pane, "Enter"],
            "Failed to send Enter via tmux",
        )
        .await?;

        Ok(())
    }
}

impl Default for TmuxManager {
    fn default() -> Self {
        let session = env_or_string("ERGATAI_TMUX_SESSION", DEFAULT_SESSION_NAME);
        Self::new(&session)
    }
}

impl Drop for TmuxManager {
    fn drop(&mut self) {
        let agent_count = self.agents.try_read().map(|a| a.len()).unwrap_or(0);

        if agent_count > 0 {
            warn!(
                "TmuxManager dropped with {} agents still registered in session '{}'. \
                 Attempting cleanup...",
                agent_count, self.default_session
            );

            // Try to clean up the tmux session synchronously
            // Use std::process::Command since we're in a synchronous Drop context
            let result = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &self.default_session])
                .output();

            match result {
                Ok(output) if output.status.success() => {
                    info!(
                        session = %self.default_session,
                        "Successfully cleaned up tmux session on drop"
                    );
                }
                Ok(output) => {
                    warn!(
                        session = %self.default_session,
                        status = %output.status,
                        stderr = %String::from_utf8_lossy(&output.stderr),
                        "Failed to kill tmux session on drop (may already be gone)"
                    );
                }
                Err(e) => {
                    warn!(
                        session = %self.default_session,
                        error = %e,
                        "Failed to execute tmux command on drop"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parameter validation ──

    #[test]
    fn test_validate_agent_id_valid() {
        assert!(validate_agent_id("agent-1").is_ok());
        assert!(validate_agent_id("opencode@9c15c5e4").is_ok());
        assert!(validate_agent_id("pane_0").is_ok());
    }

    #[test]
    fn test_validate_agent_id_empty() {
        assert!(validate_agent_id("").is_err());
    }

    #[test]
    fn test_validate_agent_id_too_long() {
        let long_id = "a".repeat(300);
        assert!(validate_agent_id(&long_id).is_err());
    }

    #[test]
    fn test_validate_agent_id_control_chars() {
        assert!(validate_agent_id("agent\n1").is_err());
        assert!(validate_agent_id("agent 1").is_err());
        assert!(validate_agent_id("agent\t1").is_err());
    }

    #[test]
    fn test_validate_tmux_pane_valid() {
        assert!(validate_tmux_pane("%0").is_ok());
        assert!(validate_tmux_pane("%123").is_ok());
        assert!(validate_tmux_pane("ergatai:0.0").is_ok());
        assert!(validate_tmux_pane("ergatai-opencode:0.1").is_ok());
    }

    #[test]
    fn test_validate_tmux_pane_invalid() {
        assert!(validate_tmux_pane("").is_err());
        assert!(validate_tmux_pane("garbage").is_err());
        assert!(validate_tmux_pane("%").is_err());
        assert!(validate_tmux_pane("%abc").is_err());
    }

    #[test]
    fn test_validate_command_valid() {
        assert!(validate_command("claude").is_ok());
        assert!(validate_command("cd /tmp && opencode").is_ok());
    }

    #[test]
    fn test_validate_command_empty() {
        assert!(validate_command("").is_err());
    }

    #[test]
    fn test_validate_command_newline() {
        assert!(validate_command("echo hi\nrm -rf /").is_err());
    }

    // ── Message sanitization ──

    #[test]
    fn test_sanitize_simple_message() {
        assert_eq!(sanitize_message("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_strips_newlines() {
        let result = sanitize_message("line1\nline2\rline3");
        assert_eq!(result, "line1 line2 line3");
        assert!(!result.contains('\n'));
        assert!(!result.contains('\r'));
    }

    #[test]
    fn test_sanitize_strips_ansi() {
        let result = sanitize_message("\x1b[31mRED\x1b[0m normal");
        assert_eq!(result, "RED normal");
    }

    #[test]
    fn test_sanitize_truncates_large_message() {
        let big = "x".repeat(MAX_MESSAGE_SIZE + 100);
        let result = sanitize_message(&big);
        assert!(result.len() <= MAX_MESSAGE_SIZE + 20);
        assert!(result.ends_with("[truncated]"));
    }

    #[test]
    fn test_sanitize_prevents_injection() {
        let malicious = "echo safe\nrm -rf /\necho also safe";
        let result = sanitize_message(malicious);
        assert!(!result.contains('\n'));
    }

    // ── ANSI stripping ──

    #[test]
    fn test_strip_ansi_colors() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_cursor() {
        assert_eq!(strip_ansi("\x1b[2J\x1b[H hello"), " hello");
    }

    #[test]
    fn test_strip_ansi_no_escapes() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    // ── TmuxManager construction ──

    #[test]
    fn test_new_session_name() {
        let mgr = TmuxManager::new("test-session");
        assert_eq!(mgr.default_session, "test-session");
    }

    #[tokio::test]
    async fn test_list_agents_empty() {
        let mgr = TmuxManager::new("test-empty");
        assert!(mgr.list_agents().await.is_empty());
    }

    #[tokio::test]
    async fn test_get_agent_missing() {
        let mgr = TmuxManager::new("test-missing");
        assert!(mgr.get_agent("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_is_agent_in_tmux_false() {
        let mgr = TmuxManager::new("test");
        assert!(!mgr.is_agent_in_tmux("ghost").await);
    }

    // ── MCP mapping ──

    #[tokio::test]
    async fn test_get_tmux_pane_no_mapping() {
        let mgr = TmuxManager::new("test");
        assert!(mgr.get_tmux_pane_for_mcp_agent("unknown").await.is_none());
    }

    #[tokio::test]
    async fn test_register_mapping_validates_pane() {
        let mgr = TmuxManager::new("test");
        let result = mgr.register_mcp_to_tmux_mapping("agent-1", "invalid").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_mapping_validates_agent_id() {
        let mgr = TmuxManager::new("test");
        let result = mgr.register_mcp_to_tmux_mapping("", "%0").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_and_lookup_mapping() {
        let mgr = TmuxManager::new("test");
        mgr.register_mcp_to_tmux_mapping("agent-1", "%0")
            .await
            .unwrap();
        let pane = mgr.get_tmux_pane_for_mcp_agent("agent-1").await;
        assert_eq!(pane, Some("%0".to_string()));
    }

    // ── try_claim_unmapped_pane ──

    #[tokio::test]
    async fn test_try_claim_no_agents() {
        let mgr = TmuxManager::new("test");
        assert!(mgr.try_claim_unmapped_pane("mcp-agent").await.is_none());
    }

    #[tokio::test]
    async fn test_try_claim_success() {
        let mgr = TmuxManager::new("test");
        mgr.agents.write().await.insert(
            "pane_0".to_string(),
            TmuxAgent {
                agent_id: "pane_0".to_string(),
                session: "test".to_string(),
                pane: "%0".to_string(),
                command: "bash".to_string(),
                mapped_to_mcp: None,
            },
        );

        let result = mgr.try_claim_unmapped_pane("mcp-1").await;
        assert_eq!(result, Some("%0".to_string()));

        // Second claim should fail — no more unmapped panes
        let result2 = mgr.try_claim_unmapped_pane("mcp-2").await;
        assert!(result2.is_none());
    }

    // ── inject_message validation ──

    #[tokio::test]
    async fn test_inject_message_agent_not_found() {
        let mgr = TmuxManager::new("test");
        let result = mgr.inject_message("ghost", "hello").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_inject_by_mcp_id_no_mapping() {
        let mgr = TmuxManager::new("test");
        let result = mgr.inject_message_by_mcp_id("unknown", "hello").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No tmux pane mapped"));
    }

    // ── stop_agent validation ──

    #[tokio::test]
    async fn test_stop_agent_not_found() {
        let mgr = TmuxManager::new("test");
        let result = mgr.stop_agent("ghost").await;
        assert!(result.is_err());
    }

    // ── check_tmux ──

    #[tokio::test]
    async fn test_check_tmux() {
        // Only passes if tmux is installed — don't assert in CI
        let result = TmuxManager::check_tmux().await;
        if result.is_err() {
            eprintln!("tmux not installed, skipping check_tmux assertion");
        }
    }
}
