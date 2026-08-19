//! RmuxBackend — rmux SDK-based agent execution environment.
//!
//! Uses the rmux daemon (a Rust terminal multiplexer) via its typed async SDK
//! (`rmux-sdk`). Each workspace maps to an rmux session; each agent maps to a
//! pane within that session. Messages are injected via `Pane::send_text()`,
//! output is captured via `Pane::screenshot()`, and lifecycle is managed via
//! `Pane::close()` / `Pane::shell()`.
//!
//! # Advantages over LocalPtyBackend
//!
//! - **Native Rust SDK** — no shelling out to `tmux` CLI commands
//! - **Structured API** — typed handles, builders, and results
//! - **Daemon-based** — persistent rmux daemon manages sessions, survive Ergatai restarts
//! - **Rich features** — snapshots, foreground state, pane event streams, layouts
//!
//! # Design
//!
//! - **Workspace = Session**: Each workspace creates an rmux session named `{prefix}-{workspace_id}`.
//! - **Agent = Pane**: Each agent gets a pane within the session. The first agent reuses
//!   pane(0,0); subsequent agents split from the previous agent's pane.
//! - **Lazy daemon connection**: The rmux daemon is connected on first use via
//!   `Rmux::builder().connect_or_start()`.
//! - **Pane handles stored locally**: `Pane` is `Clone + Send + Sync`, so we store
//!   handles in a `HashMap<String, Pane>` keyed by agent_id.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use ergatai_error::{ErgataiError, ErgataiResult};

use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, Pane, PaneExitState, PaneId, PaneProcessState,
    PaneStateClosedReason, PaneStateEvent, PaneStateEventStream, PaneStateEventsOptions, Rmux,
    RmuxEndpoint, Session, SessionName, SplitDirection, TerminalSizeSpec,
};

use crate::backend::AgentRuntimeBackend;
use crate::types::{AgentHandle, BackendCapabilities, WaitResult, WorkspaceHandle, WorkspaceSpec};

// ── Configuration constants ──

/// Maximum message size for injection (64 KiB).
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Default timeout for rmux SDK operations.
const RMUX_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default terminal dimensions.
const DEFAULT_WIDTH: u16 = 200;
const DEFAULT_HEIGHT: u16 = 50;

/// Delay before injecting instructions after agent start.
const INSTRUCTION_DELAY: Duration = Duration::from_secs(2);

/// Default timeout for text waiting operations.
const TEXT_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Common task completion markers (agent-specific prompts that indicate readiness).
const DEFAULT_COMPLETION_MARKERS: &[&str] = &[
    "How can I help you",
    "What would you like",
    "Ready",
    "$",
    ">",
];

// ── RmuxBackend ──

/// rmux SDK-based agent execution backend.
///
/// Each workspace is an rmux session. Each agent is a pane within that session.
/// Session names follow the pattern `{prefix}-{workspace_id}`.
pub struct RmuxBackend {
    /// Session name prefix (e.g., "ergatai")
    session_prefix: String,
    /// Default terminal dimensions
    width: u16,
    height: u16,
    /// Daemon endpoint (Unix socket path). `None` = platform default.
    endpoint: RmuxEndpoint,
    /// rmux daemon connection (lazy init, wrapped in Arc for shared access)
    rmux: Arc<Mutex<Option<Arc<Rmux>>>>,
    /// Active panes keyed by agent_id
    panes: Arc<RwLock<HashMap<String, Pane>>>,
    /// Per-workspace "anchor pane" — the pane we split from for the next agent.
    /// This creates a linear layout: [agent1][agent2][agent3]...
    anchor_panes: Arc<RwLock<HashMap<String, Pane>>>,
    /// Per-workspace work_dir cache — populated by create_workspace so that
    /// list_workspaces can return work_dir in metadata, and start_agent can
    /// find it even when reusing an existing workspace.
    work_dir_cache: Arc<RwLock<HashMap<String, String>>>,
    /// Workspaces created during this server run — used by start_agent to
    /// distinguish "just created" from "pre-existing" sessions. This prevents
    /// reattaching to the default shell in a freshly created session.
    fresh_workspaces: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Last time a health check was performed on the rmux connection.
    /// Used to avoid checking on every call — only check if stale (30s threshold).
    last_health_check: Arc<Mutex<std::time::Instant>>,
}

impl RmuxBackend {
    /// Create a new backend with the given session prefix.
    pub fn new(session_prefix: &str) -> Self {
        Self {
            session_prefix: session_prefix.to_string(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            endpoint: RmuxEndpoint::Default,
            rmux: Arc::new(Mutex::new(None)),
            panes: Arc::new(RwLock::new(HashMap::new())),
            anchor_panes: Arc::new(RwLock::new(HashMap::new())),
            work_dir_cache: Arc::new(RwLock::new(HashMap::new())),
            fresh_workspaces: Arc::new(RwLock::new(std::collections::HashSet::new())),
            last_health_check: Arc::new(Mutex::new(std::time::Instant::now())),
        }
    }

    /// Create with custom terminal dimensions.
    pub fn with_dimensions(mut self, width: u16, height: u16) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Create with a custom daemon endpoint (Unix socket path).
    ///
    /// Use this to connect to a specific rmux daemon instance, or to
    /// isolate multiple Ergatai instances running on the same host by
    /// giving each its own daemon socket.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let backend = RmuxBackend::new("ergatai")
    ///     .with_endpoint(RmuxEndpoint::UnixSocket("/tmp/ergatai-rmux.sock".into()));
    /// ```
    pub fn with_endpoint(mut self, endpoint: RmuxEndpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Build the full session name from prefix + workspace ID.
    /// Sanitizes the workspace ID to ensure it's safe for rmux session names.
    fn session_name(&self, workspace_id: &str) -> String {
        // Replace invalid characters with hyphens
        // Valid: alphanumeric, hyphens, underscores
        let safe_id: String = workspace_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        // Remove leading/trailing hyphens and collapse multiple hyphens
        let safe_id = safe_id
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        format!("{}-{}", self.session_prefix, safe_id)
    }

    /// Get or lazily initialize the rmux daemon connection.
    ///
    /// Returns an `Arc<Rmux>` — cheap to clone, shares the daemon connection.
    /// Uses the configured endpoint (or platform default if none set).
    ///
    /// If the cached connection is stale (daemon restarted), clears it and reconnects.
    /// Health checks are throttled to at most once per 30 seconds to avoid overhead.
    async fn get_rmux(&self) -> ErgataiResult<Arc<Rmux>> {
        let mut guard = self.rmux.lock().await;

        // Check if we have a cached connection
        if let Some(rmux) = guard.as_ref() {
            // Throttle health checks to avoid overhead on every call.
            // Only check if more than 30 seconds have passed since last check.
            const HEALTH_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
            let should_check = {
                let mut last_check = self.last_health_check.lock().await;
                if last_check.elapsed() >= HEALTH_CHECK_INTERVAL {
                    *last_check = std::time::Instant::now();
                    true
                } else {
                    false
                }
            };

            if should_check {
                // Quick health check - try a lightweight operation
                match rmux.list_sessions().await {
                    Ok(_) => return Ok(rmux.clone()),
                    Err(e) => {
                        // Connection is stale, clear it and reconnect
                        warn!(
                            error = %e,
                            "Cached rmux connection is stale, reconnecting"
                        );
                        *guard = None;
                    }
                }
            } else {
                // Skip health check, use cached connection
                return Ok(rmux.clone());
            }
        }

        info!(
            endpoint = ?self.endpoint,
            "Connecting to rmux daemon (or starting one)"
        );
        let rmux = Rmux::builder()
            .endpoint(self.endpoint.clone())
            .default_timeout(RMUX_DEFAULT_TIMEOUT)
            .connect_or_start()
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux daemon connect failed: {}", e)))?;

        info!("rmux daemon connected");
        let arc = Arc::new(rmux);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    /// Ensure a session exists for the given workspace, returning a Session handle
    /// and a boolean indicating whether the session was freshly created.
    ///
    /// Checks if the session already exists first to avoid calling `CreateOrReuse`
    /// policy, which internally uses `new-session -A` and creates a new window
    /// even when reusing an existing session.
    async fn ensure_session_handle(
        &self,
        rmux: &Rmux,
        workspace_id: &str,
        work_dir: Option<&str>,
    ) -> ErgataiResult<(Session, bool)> {
        let name_str = self.session_name(workspace_id);
        let name = SessionName::new(&name_str).map_err(|e| {
            ErgataiError::internal(format!("Invalid session name '{}': {}", name_str, e))
        })?;

        // Check if session already exists — if so, attach without creating a new window.
        // This avoids the SDK's `CreateOrReuse` which internally calls `new-session -A`
        // and creates a duplicate window in the existing session.
        // `rmux.session()` uses `reuse_only()` internally — Ok means session exists.
        if let Ok(existing) = rmux.session(name.clone()).await {
            debug!(session = name_str, "Reusing existing session (attach only)");
            return Ok((existing, false));
        }

        // Session doesn't exist — create it with a default shell in pane(0,0).
        let mut builder = EnsureSession::named(name.clone())
            .detached(false)
            .size(TerminalSizeSpec::new(self.width, self.height));

        if let Some(dir) = work_dir {
            builder = builder.working_directory(dir.to_string());
        }

        // Handle TOCTOU race: if another task creates the session between our
        // check above and this create, ensure_session may fail. Retry by
        // checking if the session now exists.
        match rmux.ensure_session(builder).await {
            Ok(session) => Ok((session, true)),
            Err(e) => {
                // Check if session was created by another task
                if let Ok(existing) = rmux.session(name).await {
                    debug!(
                        session = name_str,
                        error = %e,
                        "Session created concurrently, reusing"
                    );
                    Ok((existing, false))
                } else {
                    Err(ErgataiError::internal(format!(
                        "rmux ensure_session failed: {}",
                        e
                    )))
                }
            }
        }
    }

    /// Sanitize a message for safe terminal injection.
    fn sanitize_message(message: &str) -> String {
        let single_line: String = message
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

    /// Read an environment variable from a running process via /proc/{pid}/environ.
    ///
    /// Linux-specific: reads the null-delimited environment block from procfs.
    /// Returns `None` if the process doesn't exist, permission is denied,
    /// or the variable is not set.
    fn read_proc_environ(pid: u32, var_name: &str) -> Option<String> {
        let path = format!("/proc/{}/environ", pid);
        let data = std::fs::read(&path).ok()?;
        let prefix = format!("{}=", var_name);
        data.split(|b| *b == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .find_map(|entry| entry.strip_prefix(&prefix).map(|v| v.to_string()))
    }

    /// Find an environment variable from a child process named "opencode".
    ///
    /// The startup script (bash) exec's opencode, so the bash process becomes
    /// the parent. We scan /proc/{pid}/task/{pid}/children to find the opencode
    /// process and read its environment.
    fn find_opencode_child_environ(pid: u32, var_name: &str) -> Option<String> {
        // Read the children PIDs from /proc/{pid}/task/{pid}/children
        let children_path = format!("/proc/{}/task/{}/children", pid, pid);
        let children_data = std::fs::read_to_string(&children_path).ok()?;

        for child_pid_str in children_data.split_whitespace() {
            if let Ok(child_pid) = child_pid_str.parse::<u32>() {
                // Check if this child is named "opencode"
                let comm_path = format!("/proc/{}/comm", child_pid);
                if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                    if comm.trim() == "opencode" {
                        // Found opencode process, read the env var
                        return Self::read_proc_environ(child_pid, var_name);
                    }
                }
            }
        }
        None
    }

    // ── Advanced rmux-specific capabilities (not on trait) ──

    /// Wait until specific text appears in the agent's visible terminal output.
    ///
    /// Uses rmux's daemon-side snapshot polling — much more efficient than
    /// screenshot + grep loops. Useful for detecting:
    /// - Shell prompts ("$ ", "> ")
    /// - Agent readiness markers ("Ready", "How can I help")
    /// - Error patterns ("Error:", "FATAL", "panic")
    ///
    /// Returns `Ok(true)` if text was found, `Ok(false)` on timeout.
    pub async fn wait_for_text(
        &self,
        agent_id: &str,
        text: &str,
        timeout: Option<Duration>,
    ) -> ErgataiResult<bool> {
        let pane = {
            let panes = self.panes.read().await;
            panes.get(agent_id).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", agent_id))
            })?
        };

        let effective_timeout = timeout.unwrap_or(TEXT_WAIT_TIMEOUT);

        match tokio::time::timeout(effective_timeout, pane.wait_for_text(text)).await {
            Ok(Ok(())) => {
                debug!(agent_id, text, "Text appeared in terminal");
                Ok(true)
            }
            Ok(Err(e)) => {
                warn!(agent_id, text, error = %e, "wait_for_text error");
                Err(ErgataiError::internal(format!(
                    "rmux wait_for_text failed: {}",
                    e
                )))
            }
            Err(_) => {
                debug!(
                    agent_id,
                    text,
                    timeout_secs = effective_timeout.as_secs(),
                    "Text wait timed out"
                );
                Ok(false)
            }
        }
    }

    /// Smart task completion detection: waits until one of the given patterns
    /// appears in the visible terminal, indicating the agent has finished its
    /// task and is idle / ready for new input.
    ///
    /// If `patterns` is empty, uses `DEFAULT_COMPLETION_MARKERS` (common shell
    /// prompts and agent readiness phrases).
    ///
    /// Returns `Ok(Some(marker))` with the first matched pattern, or
    /// `Ok(None)` on timeout.
    pub async fn expect_completion(
        &self,
        agent_id: &str,
        patterns: &[&str],
        timeout: Option<Duration>,
    ) -> ErgataiResult<Option<String>> {
        let markers: Vec<&str> = if patterns.is_empty() {
            DEFAULT_COMPLETION_MARKERS.to_vec()
        } else {
            patterns.to_vec()
        };

        let effective_timeout = timeout.unwrap_or(TEXT_WAIT_TIMEOUT);
        let per_marker_timeout = effective_timeout / markers.len().max(1) as u32;

        // Try each marker concurrently via tokio::select!-like loop.
        // We race them: first one to appear wins.
        let pane = {
            let panes = self.panes.read().await;
            panes.get(agent_id).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", agent_id))
            })?
        };

        // First check existing visible text (fast path via expect_visible_text).
        for &marker in &markers {
            match pane
                .expect_visible_text()
                .to_contain(marker)
                .timeout(per_marker_timeout)
                .await
            {
                Ok(_snapshot) => {
                    debug!(agent_id, marker, "Completion marker found in visible text");
                    return Ok(Some(marker.to_string()));
                }
                Err(_) => {
                    // Not visible yet, continue to next marker
                }
            }
        }

        // None matched immediately — wait for any to appear using wait_for_text
        // (daemon-side polling, efficient).
        let overall_deadline = tokio::time::Instant::now() + effective_timeout;
        loop {
            let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                debug!(agent_id, "Completion detection timed out");
                return Ok(None);
            }

            for &marker in &markers {
                let per_check = remaining / markers.len().max(1) as u32;
                match tokio::time::timeout(
                    per_check.max(Duration::from_millis(500)),
                    pane.wait_for_text(marker),
                )
                .await
                {
                    Ok(Ok(())) => {
                        debug!(agent_id, marker, "Completion marker appeared");
                        return Ok(Some(marker.to_string()));
                    }
                    _ => continue,
                }
            }
        }
    }

    /// Open an event-driven state stream for the agent's pane.
    ///
    /// Returns a `PaneStateEventStream` that emits:
    /// - `Snapshot` — initial state on open
    /// - `TitleChanged` — pane title changed (often indicates task phase changes)
    /// - `ForegroundChanged` — foreground process changed (e.g., agent spawns sub-process)
    /// - `Closed` — pane reached terminal state (Exited / DiedKept / Killed)
    ///
    /// This is the event-driven alternative to polling `is_alive()`. Use it
    /// when you need reactive monitoring of agent state.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut stream = backend.watch_state("agent-123", true).await?;
    /// while let Some(event) = stream.next().await? {
    ///     match event {
    ///         PaneStateEvent::Closed { reason, .. } => {
    ///             println!("Agent exited: {:?}", reason);
    ///             break;
    ///         }
    ///         PaneStateEvent::TitleChanged { new_title, .. } => {
    ///             println!("Agent title changed: {}", new_title);
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub async fn watch_state(
        &self,
        agent_id: &str,
        include_foreground: bool,
    ) -> ErgataiResult<PaneStateEventStream> {
        let pane = {
            let panes = self.panes.read().await;
            panes.get(agent_id).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", agent_id))
            })?
        };

        let options = PaneStateEventsOptions {
            include_title: true,
            include_options: false,
            include_foreground,
        };

        pane.state_events(options)
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux state_events failed: {}", e)))
    }

    /// Watch the agent's pane state and return the exit reason when it closes.
    ///
    /// Convenience method that combines `watch_state` with a loop that
    /// filters for `Closed` events. Uses the event-driven stream (no polling).
    ///
    /// Returns `None` if the stream ended without a `Closed` event.
    pub async fn watch_until_exit(
        &self,
        agent_id: &str,
        timeout: Option<Duration>,
    ) -> ErgataiResult<Option<PaneStateClosedReason>> {
        let mut stream = self.watch_state(agent_id, false).await?;

        let watch_future = async {
            loop {
                match stream.next().await {
                    Ok(Some(PaneStateEvent::Closed { reason, .. })) => {
                        return Ok(Some(reason));
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => return Ok(None),
                    Err(e) => {
                        return Err(ErgataiError::internal(format!("state stream error: {}", e)));
                    }
                }
            }
        };

        if let Some(dur) = timeout {
            match tokio::time::timeout(dur, watch_future).await {
                Ok(result) => result,
                Err(_) => Ok(None),
            }
        } else {
            watch_future.await
        }
    }

    /// Get the exit state of an agent that has already exited.
    ///
    /// Unlike `wait_for_exit()` (which blocks until exit), this queries the
    /// daemon for retained exit information via `Pane::info()`. Returns
    /// `None` if the agent hasn't exited yet or the pane is gone.
    pub async fn get_exit_state(&self, agent_id: &str) -> ErgataiResult<Option<PaneExitState>> {
        let pane = {
            let panes = self.panes.read().await;
            panes.get(agent_id).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", agent_id))
            })?
        };

        // pane.info() returns InfoSnapshot { sessions, windows, panes: Vec<PaneInfo> }.
        // Each PaneInfo carries exit_state. We must match by pane identity
        // (pane_index within window) to avoid returning another pane's exit state.
        let our_pane_index = pane.target().pane_index;

        match pane.info().await {
            Ok(snapshot) => {
                for pane_info in &snapshot.panes {
                    if pane_info.index == our_pane_index {
                        if let Some(exit) = &pane_info.exit_state {
                            return Ok(Some(exit.clone()));
                        }
                        // Found our pane but no exit_state yet
                        return Ok(None);
                    }
                }
                // Our pane not found in snapshot — it may have been purged
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    // ── Daemon lifecycle ──

    /// Start the rmux daemon.
    ///
    /// Uses `ergatai_binary::ensure_rmux_daemon()` to locate the binary
    /// and start it in the background. If the daemon is already running,
    /// this is a no-op.
    ///
    /// The daemon communicates via Unix domain socket (or Windows named pipe).
    /// **rmux does not bind to a TCP/IP address** — there is no "IP" to
    /// change. To isolate daemon instances, use `with_endpoint()` with a
    /// custom Unix socket path.
    pub fn start_daemon(&self) -> ErgataiResult<()> {
        ergatai_binary::ensure_rmux_daemon(true)?;
        info!("rmux daemon ensured (started if not running)");
        Ok(())
    }

    /// Stop the rmux daemon.
    ///
    /// Connects to the daemon (using the configured endpoint) and sends
    /// a shutdown request via `Rmux::shutdown()`. This is equivalent to
    /// `rmux kill-server` on the CLI.
    ///
    /// After stopping, all sessions and panes managed by the daemon are
    /// destroyed. This backend's local tracking state is also cleared.
    pub async fn stop_daemon(&self) -> ErgataiResult<()> {
        // Try to connect and shutdown gracefully
        let rmux = self.get_rmux().await?;

        info!("Sending shutdown request to rmux daemon");
        // Rmux::shutdown() consumes self, so we need to work around that.
        // We'll use the CLI command instead for a non-consuming shutdown.
        drop(rmux);

        // Use the rmux binary directly to send kill-server
        let daemon_path = ergatai_binary::get_daemon_path().ok_or_else(|| {
            ErgataiError::internal("rmux daemon binary path not configured".to_string())
        })?;

        let mut cmd = std::process::Command::new(&daemon_path);
        cmd.arg("kill-server");

        // Add socket path if using a custom endpoint
        if let RmuxEndpoint::UnixSocket(ref path) = self.endpoint {
            cmd.arg("-S").arg(path);
        }

        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    info!("rmux daemon stopped via kill-server");
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        stderr = %stderr,
                        "rmux kill-server returned non-zero (daemon may already be stopped)"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to execute rmux kill-server");
            }
        }

        // Clear local connection state
        *self.rmux.lock().await = None;
        self.panes.write().await.clear();
        self.anchor_panes.write().await.clear();

        Ok(())
    }

    /// Restart the rmux daemon (stop + start).
    ///
    /// Useful after changing configuration or when the daemon is in a
    /// bad state. Clears all local tracking state.
    pub async fn restart_daemon(&self) -> ErgataiResult<()> {
        self.stop_daemon().await?;
        // Brief pause to let the socket file clean up
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.start_daemon()?;
        info!("rmux daemon restarted");
        Ok(())
    }

    // ── Daemon status ──

    /// Check whether the rmux daemon binary is available on this host.
    ///
    /// This does NOT connect to a running daemon — it only verifies the binary
    /// can be found via `ergatai_binary` (env var → bundled → PATH). Use
    /// `is_daemon_connected()` to check whether we have an active connection.
    pub fn is_daemon_available(&self) -> bool {
        ergatai_binary::is_rmux_available()
    }

    /// Check whether we currently have an active connection to the daemon.
    ///
    /// Returns `false` if the lazy connection has not been established yet.
    /// Does NOT attempt to connect — call `initialize()` for that.
    pub async fn is_daemon_connected(&self) -> bool {
        self.rmux.lock().await.is_some()
    }

    /// Inject a message into any pane known to the daemon, identified by
    /// session name and pane id.
    ///
    /// Unlike `inject_message()` (which requires the pane to be tracked in
    /// `self.panes`), this method reconstructs a lightweight `Pane` handle
    /// from the daemon on demand. This enables messaging to panes that were:
    /// - Discovered via `daemon_info()` / `find_panes()` but not launched by this backend
    /// - Created by another Ergatai process or manually by the user
    /// - Survived a backend restart (daemon persists, local tracking does not)
    ///
    /// # Arguments
    ///
    /// * `session_name` — rmux session name (e.g., `"ergatai-task-123"`)
    /// * `pane_id` — stable pane identity in `"%N"` format (e.g., `"%3"`)
    /// * `message` — text to inject (newlines stripped, truncated to 64 KiB)
    pub async fn inject_message_to_pane(
        &self,
        session_name: &str,
        pane_id: &str,
        message: &str,
    ) -> ErgataiResult<()> {
        let rmux = self.get_rmux().await?;

        let sname = SessionName::new(session_name).map_err(|e| {
            ErgataiError::internal(format!("Invalid session name '{}': {}", session_name, e))
        })?;

        let raw_id = pane_id.strip_prefix('%').ok_or_else(|| {
            ErgataiError::internal(format!(
                "Invalid pane_id '{}': must start with '%'",
                pane_id
            ))
        })?;
        let pid = PaneId::new(raw_id.parse::<u32>().map_err(|e| {
            ErgataiError::internal(format!("Invalid pane_id '{}': {}", pane_id, e))
        })?);

        let pane = rmux.pane_by_id(sname, pid).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to get pane {} in session {}: {}",
                pane_id, session_name, e
            ))
        })?;

        let sanitized = Self::sanitize_message(message);
        // Send text first, then Enter as a separate key event (same as inject_message)
        pane.send_text(sanitized).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to inject message into pane {}: {}",
                pane_id, e
            ))
        })?;

        pane.send_key("Enter").await.map_err(|e| {
            ErgataiError::internal(format!("Failed to send Enter to pane {}: {}", pane_id, e))
        })?;

        debug!(
            session = session_name,
            pane_id = pane_id,
            bytes = message.len(),
            "Message injected via daemon-discovered pane"
        );
        Ok(())
    }

    /// Close any pane known to the daemon, identified by session name and
    /// pane id.
    ///
    /// Unlike `stop_agent()` (which requires the pane to be tracked in
    /// `self.panes`), this method reconstructs a lightweight `Pane` handle
    /// from the daemon on demand and closes it. This enables stopping panes
    /// that were discovered via `daemon_info()` but not launched by this
    /// backend, or that survived a backend restart.
    ///
    /// # Arguments
    ///
    /// * `session_name` — rmux session name (e.g., `"ergatai-task-123"`)
    /// * `pane_id` — stable pane identity in `"%N"` format (e.g., `"%3"`)
    pub async fn stop_pane(&self, session_name: &str, pane_id: &str) -> ErgataiResult<()> {
        let rmux = self.get_rmux().await?;

        let sname = SessionName::new(session_name).map_err(|e| {
            ErgataiError::internal(format!("Invalid session name '{}': {}", session_name, e))
        })?;

        let raw_id = pane_id.strip_prefix('%').ok_or_else(|| {
            ErgataiError::internal(format!(
                "Invalid pane_id '{}': must start with '%'",
                pane_id
            ))
        })?;
        let pid = PaneId::new(raw_id.parse::<u32>().map_err(|e| {
            ErgataiError::internal(format!("Invalid pane_id '{}': {}", pane_id, e))
        })?);

        let pane = rmux.pane_by_id(sname, pid).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to get pane {} in session {}: {}",
                pane_id, session_name, e
            ))
        })?;

        info!(
            session = session_name,
            pane_id = pane_id,
            "Stopping daemon-discovered pane"
        );

        match pane.close().await {
            Ok(outcome) => {
                debug!(
                    session = session_name,
                    pane_id = pane_id,
                    ?outcome,
                    "Daemon-discovered pane closed"
                );
            }
            Err(e) => {
                warn!(
                    session = session_name,
                    pane_id = pane_id,
                    error = %e,
                    "Failed to close daemon-discovered pane (may already be closed)"
                );
            }
        }

        Ok(())
    }

    /// Force-kill any pane known to the daemon.
    ///
    /// For rmux, `kill_pane` and `stop_pane` are the same — closing the pane
    /// causes the daemon to send SIGKILL to the pane's foreground process.
    pub async fn kill_pane(&self, session_name: &str, pane_id: &str) -> ErgataiResult<()> {
        self.stop_pane(session_name, pane_id).await
    }

    /// Gracefully stop a daemon-discovered pane by sending an exit command
    /// first, then closing the pane if the process doesn't exit on its own.
    ///
    /// # Flow
    ///
    /// 1. Send `exit_command` as text into the pane (e.g., `"/exit\n"`, `"exit\n"`)
    ///    or a special key (e.g., `"C-c"` for Ctrl+C)
    /// 2. Wait up to `grace_period` for the process to exit
    /// 3. If still running after grace period, close the pane (force kill)
    ///
    /// # Arguments
    ///
    /// * `session_name` — rmux session name
    /// * `pane_id` — stable pane identity (`"%N"` format)
    /// * `exit_command` — text to send (e.g., `"/exit\n"`, `"exit\n"`) or a
    ///   tmux key token (e.g., `"C-c"` for Ctrl+C, `"C-d"` for Ctrl+D)
    /// * `grace_period` — how long to wait for clean exit before force-closing.
    ///   Pass `None` for a 5-second default.
    pub async fn graceful_stop_pane(
        &self,
        session_name: &str,
        pane_id: &str,
        exit_command: &str,
        grace_period: Option<Duration>,
    ) -> ErgataiResult<()> {
        let rmux = self.get_rmux().await?;
        let grace = grace_period.unwrap_or(Duration::from_secs(5));

        let sname = SessionName::new(session_name).map_err(|e| {
            ErgataiError::internal(format!("Invalid session name '{}': {}", session_name, e))
        })?;

        let raw_id = pane_id.strip_prefix('%').ok_or_else(|| {
            ErgataiError::internal(format!(
                "Invalid pane_id '{}': must start with '%'",
                pane_id
            ))
        })?;
        let pid = PaneId::new(raw_id.parse::<u32>().map_err(|e| {
            ErgataiError::internal(format!("Invalid pane_id '{}': {}", pane_id, e))
        })?);

        let pane = rmux.pane_by_id(sname, pid).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to get pane {} in session {}: {}",
                pane_id, session_name, e
            ))
        })?;

        // Step 1: Send exit command
        info!(
            session = session_name,
            pane_id = pane_id,
            command = exit_command,
            "Sending graceful exit command"
        );

        // Detect if this is a tmux key token (short, no spaces, no newlines)
        let is_key_token = exit_command.len() <= 8
            && !exit_command.contains(' ')
            && !exit_command.contains('\n')
            && !exit_command.contains('\r');

        if is_key_token
            && (exit_command.starts_with("C-")
                || exit_command.starts_with("M-")
                || exit_command.eq_ignore_ascii_case("Enter")
                || exit_command.eq_ignore_ascii_case("Escape")
                || exit_command.eq_ignore_ascii_case("Tab")
                || exit_command.eq_ignore_ascii_case("BSpace")
                || exit_command.eq_ignore_ascii_case("Space"))
        {
            pane.send_key(exit_command).await.map_err(|e| {
                ErgataiError::internal(format!("Failed to send key '{}': {}", exit_command, e))
            })?;
        } else {
            let sanitized = Self::sanitize_message(exit_command);
            pane.send_text(sanitized).await.map_err(|e| {
                ErgataiError::internal(format!(
                    "Failed to send exit command '{}': {}",
                    exit_command, e
                ))
            })?;
        }

        // Step 2: Wait for the process to exit
        match tokio::time::timeout(grace, pane.wait_for_exit()).await {
            Ok(Ok(_)) => {
                debug!(
                    session = session_name,
                    pane_id = pane_id,
                    "Pane exited gracefully after exit command"
                );
                return Ok(());
            }
            Ok(Err(e)) => {
                warn!(
                    session = session_name,
                    pane_id = pane_id,
                    error = %e,
                    "Error waiting for pane exit"
                );
            }
            Err(_) => {
                debug!(
                    session = session_name,
                    pane_id = pane_id,
                    grace_secs = grace.as_secs(),
                    "Pane did not exit within grace period, force-closing"
                );
            }
        }

        // Step 3: Force close
        match pane.close().await {
            Ok(outcome) => {
                debug!(
                    session = session_name,
                    pane_id = pane_id,
                    ?outcome,
                    "Pane force-closed after grace period"
                );
            }
            Err(e) => {
                warn!(
                    session = session_name,
                    pane_id = pane_id,
                    error = %e,
                    "Failed to force-close pane"
                );
            }
        }

        Ok(())
    }

    /// Gracefully stop a locally-tracked agent by sending an exit command
    /// first, then closing the pane if the process doesn't exit on its own.
    ///
    /// Same flow as `graceful_stop_pane()` but uses the local pane tracking
    /// map instead of daemon discovery.
    pub async fn graceful_stop_agent(
        &self,
        handle: &AgentHandle,
        exit_command: &str,
        grace_period: Option<Duration>,
    ) -> ErgataiResult<()> {
        let key = handle.agent_id.clone();
        let grace = grace_period.unwrap_or(Duration::from_secs(5));

        let pane = {
            let panes = self.panes.read().await;
            panes.get(&key).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", key))
            })?
        };

        // Step 1: Send exit command
        info!(
            agent_id = key,
            command = exit_command,
            "Sending graceful exit command"
        );

        let is_key_token = exit_command.len() <= 8
            && !exit_command.contains(' ')
            && !exit_command.contains('\n')
            && !exit_command.contains('\r');

        if is_key_token
            && (exit_command.starts_with("C-")
                || exit_command.starts_with("M-")
                || exit_command.eq_ignore_ascii_case("Enter")
                || exit_command.eq_ignore_ascii_case("Escape")
                || exit_command.eq_ignore_ascii_case("Tab")
                || exit_command.eq_ignore_ascii_case("BSpace")
                || exit_command.eq_ignore_ascii_case("Space"))
        {
            pane.send_key(exit_command).await.map_err(|e| {
                ErgataiError::internal(format!("Failed to send key '{}': {}", exit_command, e))
            })?;
        } else {
            let sanitized = Self::sanitize_message(exit_command);
            pane.send_text(sanitized).await.map_err(|e| {
                ErgataiError::internal(format!(
                    "Failed to send exit command '{}': {}",
                    exit_command, e
                ))
            })?;
        }

        // Step 2: Wait for the process to exit
        match tokio::time::timeout(grace, pane.wait_for_exit()).await {
            Ok(Ok(_)) => {
                debug!(agent_id = key, "Agent exited gracefully after exit command");
                // Remove from local tracking
                self.panes.write().await.remove(&key);
                return Ok(());
            }
            Ok(Err(e)) => {
                warn!(agent_id = key, error = %e, "Error waiting for agent exit");
            }
            Err(_) => {
                debug!(
                    agent_id = key,
                    grace_secs = grace.as_secs(),
                    "Agent did not exit within grace period, force-closing"
                );
            }
        }

        // Step 3: Force close
        self.panes.write().await.remove(&key);
        match pane.close().await {
            Ok(outcome) => {
                debug!(agent_id = key, ?outcome, "Agent pane force-closed");
            }
            Err(e) => {
                warn!(agent_id = key, error = %e, "Failed to force-close agent pane");
            }
        }

        Ok(())
    }

    /// Capture the current visible terminal content of a daemon-discovered pane.
    ///
    /// Unlike `capture_output()` (which requires the pane to be tracked in
    /// `self.panes`), this method reconstructs a lightweight `Pane` handle
    /// from the daemon on demand. Returns the text content of the pane's
    /// current visible screen (the terminal viewport, not scrollback).
    ///
    /// # Arguments
    ///
    /// * `session_name` — rmux session name (e.g., `"ergatai-task-123"`)
    /// * `pane_id` — stable pane identity in `"%N"` format (e.g., `"%3"`)
    pub async fn capture_pane_output(
        &self,
        session_name: &str,
        pane_id: &str,
    ) -> ErgataiResult<String> {
        let rmux = self.get_rmux().await?;

        let sname = SessionName::new(session_name).map_err(|e| {
            ErgataiError::internal(format!("Invalid session name '{}': {}", session_name, e))
        })?;

        let raw_id = pane_id.strip_prefix('%').ok_or_else(|| {
            ErgataiError::internal(format!(
                "Invalid pane_id '{}': must start with '%'",
                pane_id
            ))
        })?;
        let pid = PaneId::new(raw_id.parse::<u32>().map_err(|e| {
            ErgataiError::internal(format!("Invalid pane_id '{}': {}", pane_id, e))
        })?);

        let pane = rmux.pane_by_id(sname, pid).await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to get pane {} in session {}: {}",
                pane_id, session_name, e
            ))
        })?;

        let captured = pane.screenshot().await.map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to capture pane {} in session {}: {}",
                pane_id, session_name, e
            ))
        })?;

        debug!(
            session = session_name,
            pane_id = pane_id,
            bytes = captured.text.len(),
            "Pane output captured via daemon"
        );

        Ok(captured.text)
    }

    /// Snapshot of daemon status, useful for diagnostics and the CLI
    /// `ergatai daemon status` command.
    ///
    /// Queries the rmux daemon for **real state** — does not rely on the
    /// backend's local tracking maps. All data comes from daemon protocol
    /// calls: `list_sessions()` for session enumeration and `find_panes()`
    /// for per-pane discovery (title, command, cwd, process state).
    ///
    /// This means `daemon_info()` works correctly even if:
    /// - The backend was freshly created (no local panes tracked yet)
    /// - Another process created ergatai sessions that this backend didn't launch
    /// - Panes were created outside the backend's awareness
    pub async fn daemon_info(&self) -> RmuxDaemonInfo {
        let binary_available = self.is_daemon_available();
        let connected = self.is_daemon_connected().await;
        let tracked_pane_count = self.panes.read().await.len();
        let tracked_workspace_count = self.anchor_panes.read().await.len();

        let binary_path = ergatai_binary::configure_rmux_daemon().ok();

        // Query real daemon state if connected — fully daemon-driven, no
        // dependency on local self.panes or self.anchor_panes tracking.
        let mut total_sessions: usize = 0;
        let mut total_daemon_panes: usize = 0;
        let mut ergatai_sessions: Vec<String> = Vec::new();
        let mut managed_panes: Vec<ManagedPaneInfo> = Vec::new();

        if connected {
            if let Ok(rmux) = self.get_rmux().await {
                let prefix = format!("{}-", self.session_prefix);

                // 1. List all daemon sessions (single protocol call)
                if let Ok(session_names) = rmux.list_sessions().await {
                    total_sessions = session_names.len();

                    for name in &session_names {
                        let name_str = name.as_str().to_string();
                        if name_str.starts_with(&prefix) {
                            ergatai_sessions.push(name_str);
                        }
                    }
                }

                // 2. Discover all panes from the daemon (uses list-panes +
                //    per-pane info/title internally — fully daemon-driven)
                if let Ok(all_discovered) = rmux.find_panes().all().await {
                    total_daemon_panes = all_discovered.len();

                    // 3. Filter to ergatai-owned sessions and build details
                    for dp in &all_discovered {
                        if !dp.session_name.as_str().starts_with(&prefix) {
                            continue;
                        }

                        let process_state = match &dp.process {
                            PaneProcessState::Running { pid } => {
                                if pid.is_some() {
                                    "running"
                                } else {
                                    "running (pid unknown)"
                                }
                            }
                            PaneProcessState::Exited => "exited",
                            PaneProcessState::Unknown => "unknown",
                            _ => "unknown",
                        };

                        let pid = match &dp.process {
                            PaneProcessState::Running { pid } => *pid,
                            _ => None,
                        };

                        // Query exit_state from daemon (cheap — info() on a
                        // single pane re-reads the daemon's retained state)
                        let (exit_code, exit_signal) = if dp.process == PaneProcessState::Exited {
                            match dp.pane.info().await {
                                Ok(snapshot) => {
                                    let exit = snapshot
                                        .pane(dp.pane_id)
                                        .and_then(|pi| pi.exit_state.as_ref());
                                    (exit.and_then(|e| e.code), exit.and_then(|e| e.signal))
                                }
                                Err(_) => (None, None),
                            }
                        } else {
                            (None, None)
                        };

                        managed_panes.push(ManagedPaneInfo {
                            session_name: dp.session_name.as_str().to_string(),
                            pane_id: dp.pane_id.to_string(),
                            pane_index: dp.pane_index,
                            command: dp.command.clone(),
                            working_directory: dp.working_directory.clone(),
                            process_state: process_state.to_string(),
                            pid,
                            exit_code,
                            exit_signal,
                        });
                    }
                }
            }
        }

        RmuxDaemonInfo {
            binary_available,
            binary_path,
            connected,
            tracked_pane_count,
            tracked_workspace_count,
            total_sessions,
            total_daemon_panes,
            ergatai_sessions,
            managed_panes,
        }
    }
}

/// Snapshot of rmux daemon state — returned by `RmuxBackend::daemon_info()`.
///
/// Contains both local tracking state (what this backend instance manages)
/// and real daemon state (queried from the rmux daemon on each call).
#[derive(Debug, Clone, Serialize)]
pub struct RmuxDaemonInfo {
    /// Whether the daemon binary can be located (env var → bundled → PATH).
    pub binary_available: bool,
    /// Resolved path to the daemon binary (if found).
    pub binary_path: Option<std::path::PathBuf>,
    /// Whether this backend has an active daemon connection.
    pub connected: bool,
    /// Number of pane handles currently tracked by this backend instance.
    pub tracked_pane_count: usize,
    /// Number of workspaces (sessions) tracked by this backend instance.
    pub tracked_workspace_count: usize,
    /// Total sessions known to the daemon (all prefixes, not just ergatai).
    pub total_sessions: usize,
    /// Total panes known to the daemon across all sessions.
    pub total_daemon_panes: usize,
    /// Session names owned by this backend (matching the session prefix).
    pub ergatai_sessions: Vec<String>,
    /// Per-pane details for all panes in ergatai-owned sessions.
    pub managed_panes: Vec<ManagedPaneInfo>,
}

/// Details for a single pane managed by this backend, queried from the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedPaneInfo {
    /// Session name this pane belongs to (e.g., "ergatai-task-123").
    pub session_name: String,
    /// Stable pane identity (e.g., "%3").
    pub pane_id: String,
    /// Pane index within its window.
    pub pane_index: u32,
    /// Spawned process argv recorded by the daemon (e.g., ["claude", "--resume"]).
    pub command: Option<Vec<String>>,
    /// Process working directory at time of the snapshot.
    pub working_directory: Option<String>,
    /// Process state: "running", "exited", or "unknown".
    pub process_state: String,
    /// OS process ID, if the daemon could resolve it and the process is running.
    pub pid: Option<u32>,
    /// Exit code, if the process has exited normally.
    pub exit_code: Option<i32>,
    /// Exit signal, if the process was killed by a signal.
    pub exit_signal: Option<i32>,
}

#[async_trait]
impl AgentRuntimeBackend for RmuxBackend {
    fn name(&self) -> &'static str {
        "rmux"
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
        let rmux = self.get_rmux().await?;
        // Verify daemon is responsive by creating and destroying a probe session
        let probe_name = SessionName::new("__ergatai_probe__")
            .map_err(|e| ErgataiError::internal(format!("Invalid probe name: {}", e)))?;
        let session = rmux
            .ensure_session(
                EnsureSession::named(probe_name)
                    .policy(EnsureSessionPolicy::CreateOrReuse)
                    .detached(true)
                    .size(TerminalSizeSpec::new(80, 24)),
            )
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux probe failed: {}", e)))?;

        // Clean up the probe session immediately
        let _ = session.kill().await;
        info!("rmux backend initialized (daemon connected)");
        Ok(())
    }

    /// Discover agents running in rmux panes across ALL sessions.
    ///
    /// Scans the rmux daemon for all sessions and their panes, filtering to
    /// only running panes. Each discovered pane is registered with an
    /// `AgentHandle` that stores the `Pane` object in `self.panes` so that
    /// `inject_message()` can later deliver messages to it.
    ///
    /// Unlike `LocalPtyBackend`, this does NOT filter by session prefix —
    /// it discovers agents in ANY rmux session, enabling dynamic discovery
    /// of manually-started agents.
    async fn discover_agents(&self) -> ErgataiResult<Vec<(String, AgentHandle)>> {
        let rmux = self.get_rmux().await?;

        // List all sessions from the daemon
        let session_names = rmux
            .list_sessions()
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to list rmux sessions: {}", e)))?;

        info!(
            sessions = session_names.len(),
            "Scanning rmux daemon for running agents"
        );

        // Find all panes across all sessions
        let all_panes = rmux
            .find_panes()
            .all()
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to find rmux panes: {}", e)))?;

        let mut discovered = Vec::new();

        // Collect all pane data WITHOUT holding the write lock.
        // This prevents blocking concurrent reads (inject_message, etc.) during discovery.
        let mut panes_to_insert: Vec<(String, Pane)> = Vec::new();

        for dp in &all_panes {
            // Only consider running panes (skip exited/unknown)
            let child_pid = match &dp.process {
                PaneProcessState::Running { pid: Some(pid) } => Some(*pid),
                PaneProcessState::Running { pid: None } => None,
                _ => continue, // skip non-running panes
            };

            let session_name = dp.session_name.as_str().to_string();

            // Skip internal sessions (names starting with `_`) — these are
            // warmup/keepalive sessions, not agent panes.
            if session_name.starts_with('_') {
                continue;
            }

            let pane_id = format!("%{}", dp.pane_id.as_u32());
            let command = dp
                .command
                .as_ref()
                .and_then(|c| c.first().cloned())
                .unwrap_or_default();

            // Try to read RMUX_PANE from the pane's child process environment.
            // This gives us the deterministic pane identifier (e.g., "%15").
            let rmux_pane = child_pid.and_then(|pid| Self::read_proc_environ(pid, "RMUX_PANE"));

            // Try to read ERGATAI_AGENT_ID from the pane's descendant processes.
            // The startup script sets ERGATAI_AGENT_ID, then exec's opencode.
            // We need to find the opencode process (child of bash) to read this env var.
            let ergatai_agent_id = child_pid.and_then(|pid| {
                // First try the direct child (bash process)
                if let Some(id) = Self::read_proc_environ(pid, "ERGATAI_AGENT_ID") {
                    tracing::info!(pid = pid, agent_id = %id, "Found ERGATAI_AGENT_ID in direct child");
                    return Some(id);
                }
                // If not found, look for opencode child process
                let result = Self::find_opencode_child_environ(pid, "ERGATAI_AGENT_ID");
                if let Some(ref id) = result {
                    tracing::info!(parent_pid = pid, agent_id = %id, "Found ERGATAI_AGENT_ID in opencode child");
                } else {
                    tracing::info!(parent_pid = pid, "ERGATAI_AGENT_ID not found in any child process");
                }
                result
            });

            // Use the deterministic RMUX_PANE identifier (e.g., "%15") as agent_id.
            // Fall back to pane_id (e.g., "%0") if RMUX_PANE can't be read from /proc.
            // This ensures the same pane always gets the same agent_id across scans,
            // making discover_and_register_agents() idempotent.
            let agent_id = rmux_pane.clone().unwrap_or_else(|| pane_id.clone());

            let mut metadata = HashMap::new();
            metadata.insert("session".to_string(), session_name.clone());
            metadata.insert("pane_id".to_string(), pane_id.clone());
            if let Some(ref rp) = rmux_pane {
                metadata.insert("rmux_pane".to_string(), rp.clone());
            }
            if let Some(ref eai) = ergatai_agent_id {
                metadata.insert("ergatai_agent_id".to_string(), eai.clone());
            }

            info!(
                agent_id = agent_id,
                session = session_name,
                pane_id = pane_id,
                rmux_pane = rmux_pane.as_deref().unwrap_or("unknown"),
                pid = child_pid
                    .map(|p| p.to_string())
                    .as_deref()
                    .unwrap_or("unknown"),
                command = command,
                "Discovered agent in rmux pane"
            );

            let workspace = WorkspaceHandle {
                id: session_name.clone(),
                backend: "rmux".to_string(),
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("session".to_string(), session_name);
                    m
                },
            };

            let handle_agent_id = agent_id.clone();
            panes_to_insert.push((agent_id, dp.pane.clone()));

            discovered.push((
                handle_agent_id.clone(),
                AgentHandle {
                    agent_id: handle_agent_id,
                    workspace,
                    process_id: child_pid.map(|p| p.to_string()),
                    metadata,
                },
            ));
        }

        // Briefly acquire the write lock to insert all discovered panes at once.
        {
            let mut panes_map = self.panes.write().await;
            for (agent_id, pane_handle) in panes_to_insert {
                panes_map.insert(agent_id, pane_handle);
            }
        }

        info!(
            count = discovered.len(),
            sessions = session_names.len(),
            "Discovery scan complete"
        );

        Ok(discovered)
    }

    async fn create_workspace(&self, spec: WorkspaceSpec) -> ErgataiResult<WorkspaceHandle> {
        let rmux = self.get_rmux().await?;
        let session_name = self.session_name(&spec.id);

        info!(
            session = session_name,
            width = self.width,
            height = self.height,
            "Creating rmux session workspace"
        );

        // Create session with detached=false to get default shell in pane(0,0).
        // launch_agent ensures this is only called once per workspace.
        let (_session, freshly_created) = self
            .ensure_session_handle(&rmux, &spec.id, Some(spec.work_dir.to_str().unwrap_or(".")))
            .await?;

        debug!(
            session = session_name,
            created = freshly_created,
            "rmux session ensured"
        );

        let mut metadata = HashMap::new();
        metadata.insert("session".to_string(), session_name.clone());
        // Store work_dir in metadata so start_agent can use it for .cwd()
        metadata.insert(
            "work_dir".to_string(),
            spec.work_dir.to_string_lossy().to_string(),
        );

        // Cache work_dir so list_workspaces can return it and start_agent
        // can find it when reusing an existing workspace.
        self.work_dir_cache.write().await.insert(
            spec.id.clone(),
            spec.work_dir.to_string_lossy().to_string(),
        );

        // Mark as freshly created so start_agent knows to spawn (not reattach).
        // Only if the rmux session was actually created here (not reused).
        if freshly_created {
            self.fresh_workspaces.write().await.insert(spec.id.clone());
        }

        Ok(WorkspaceHandle {
            id: spec.id,
            backend: "rmux".to_string(),
            metadata,
        })
    }

    async fn start_agent(
        &self,
        handle: &WorkspaceHandle,
        command: &str,
        instruction: Option<&str>,
    ) -> ErgataiResult<AgentHandle> {
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

        let rmux = self.get_rmux().await?;
        // Read work_dir from metadata (stored during create_workspace)
        let work_dir = handle.metadata.get("work_dir").map(|s| s.as_str());
        let (session, _freshly_created) = self.ensure_session_handle(&rmux, &handle.id, work_dir).await?;
        let session_name = self.session_name(&handle.id);

        let mut anchors = self.anchor_panes.write().await;

        // Check if this workspace was JUST created in this server run.
        // If so, always spawn (the existing pane is just the default shell).
        // Otherwise, check for existing running agent panes to reattach.
        let is_fresh = self.fresh_workspaces.read().await.contains(&handle.id);
        if !is_fresh {
            let all_discovered = rmux.find_panes().all().await.unwrap_or_default();
            for dp in &all_discovered {
                if dp.session_name.as_str() == session_name {
                    if let PaneProcessState::Running { .. } = dp.process {
                        // Found an existing running pane in this session — reuse it
                        let pane = session.pane(dp.window_index, dp.pane_index);
                        info!(
                            session = session_name,
                            pane_index = dp.pane_index,
                            "Reattaching to existing agent pane"
                        );
                        anchors.entry(handle.id.clone()).or_insert_with(|| pane.clone());
                        let agent_id = format!("agent-{}", uuid::Uuid::new_v4().as_simple());
                        return Ok(AgentHandle {
                            agent_id,
                            workspace: handle.clone(),
                            process_id: None,
                            metadata: {
                                let mut m = HashMap::new();
                                m.insert("reattached".to_string(), "true".to_string());
                                m
                            },
                        });
                    }
                }
            }
        }

        let is_first = is_fresh || !anchors.contains_key(&handle.id);

        let agent_pane = if is_first {
            // First agent: respawn pane(0,0) with the agent command.
            // create_workspace already created the session with a default shell
            // in pane(0,0); we replace it with the agent command.
            let pane = session.pane(0, 0);
            let mut builder = pane
                .shell(command)
                .kill_existing(true)
                .title(format!("agent-{}", handle.id));
            if let Some(dir) = work_dir {
                builder = builder.cwd(dir);
            }
            builder
                .await
                .map_err(|e| ErgataiError::internal(format!("rmux shell spawn failed: {}", e)))?;
            pane
        } else {
            // Subsequent agents: split from the anchor pane to create a new one
            // But first check if the anchor pane still exists (it may have been cleaned up)
            let anchor = anchors.get(&handle.id).cloned();

            if let Some(anchor) = anchor {
                // Try to split from anchor; if it fails (pane no longer exists), fall back
                match anchor.split(SplitDirection::Right).await {
                    Ok(new_pane) => {
                        let mut builder = new_pane
                            .shell(command)
                            .kill_existing(true)
                            .title(format!("agent-{}", handle.id));
                        if let Some(dir) = work_dir {
                            builder = builder.cwd(dir);
                        }
                        builder
                            .await
                            .map_err(|e| ErgataiError::internal(format!("rmux shell spawn failed: {}", e)))?;
                        new_pane
                    }
                    Err(e) => {
                        // Anchor pane no longer exists, fall back to first-agent behavior
                        warn!(
                            workspace = handle.id,
                            error = %e,
                            "Anchor pane no longer exists, falling back to pane(0,0)"
                        );
                        let pane = session.pane(0, 0);
                        let mut builder = pane
                            .shell(command)
                            .kill_existing(true)
                            .title(format!("agent-{}", handle.id));
                        if let Some(dir) = work_dir {
                            builder = builder.cwd(dir);
                        }
                        builder
                            .await
                            .map_err(|e| ErgataiError::internal(format!("rmux shell respawn failed: {}", e)))?;
                        pane
                    }
                }
            } else {
                // No anchor stored, use first-agent behavior
                warn!(
                    workspace = handle.id,
                    "No anchor pane found, falling back to pane(0,0)"
                );
                let pane = session.pane(0, 0);
                let mut builder = pane
                    .shell(command)
                    .kill_existing(true)
                    .title(format!("agent-{}", handle.id));
                if let Some(dir) = work_dir {
                    builder = builder.cwd(dir);
                }
                builder
                    .await
                    .map_err(|e| ErgataiError::internal(format!("rmux shell respawn failed: {}", e)))?;
                pane
            }
        };

        // Update the anchor to the new pane (linear layout)
        anchors.insert(handle.id.clone(), agent_pane.clone());
        drop(anchors);

        // Remove from fresh_workspaces — the agent has been spawned, so
        // subsequent start_agent calls should treat this as a pre-existing session.
        self.fresh_workspaces.write().await.remove(&handle.id);

        info!(
            session = session_name,
            workspace = handle.id,
            is_first = is_first,
            "Agent started in rmux pane"
        );

        // Inject instruction if provided
        if let Some(instr) = instruction {
            tokio::time::sleep(INSTRUCTION_DELAY).await;
            let sanitized = Self::sanitize_message(instr);
            // Send text first, then Enter as a separate key event
            agent_pane.send_text(sanitized).await.map_err(|e| {
                ErgataiError::internal(format!("rmux instruction injection failed: {}", e))
            })?;
            agent_pane.send_key("Enter").await.map_err(|e| {
                ErgataiError::internal(format!("rmux instruction Enter failed: {}", e))
            })?;
            info!(
                workspace = handle.id,
                bytes = instr.len(),
                "Instruction injected"
            );
        }

        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());

        // Store the pane handle
        self.panes
            .write()
            .await
            .insert(agent_id.clone(), agent_pane);

        Ok(AgentHandle {
            workspace: handle.clone(),
            agent_id,
            process_id: None,
            metadata: HashMap::new(),
        })
    }

    async fn inject_message(&self, handle: &AgentHandle, message: &str) -> ErgataiResult<()> {
        let key = handle.agent_id.clone();
        // Clone the Pane out of the map, then drop the read guard BEFORE awaiting.
        // Pane is Clone + Send + Sync; holding the read guard across the async RPC
        // would block start_agent/stop_agent if the daemon is slow.
        let pane = {
            let panes = self.panes.read().await;
            panes.get(&key).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", key))
            })?
        };

        let sanitized = Self::sanitize_message(message);

        tracing::info!(
            agent_id = %key,
            original_len = message.len(),
            sanitized_len = sanitized.len(),
            message_preview = sanitized.get(..150).unwrap_or(&sanitized),
            "Injecting message via rmux"
        );

        // Send text first (literal typing, no Enter), then send Enter as a
        // separate key event. This matches the old TmuxManager behaviour
        // (`send-keys -l text` + `send-keys Enter`) and ensures the TUI
        // interprets the Enter as a submit action rather than pasting a
        // newline character into a multi-line input field.
        pane.send_text(&sanitized)
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux send_text failed: {}", e)))?;

        // Delay to let the terminal process the text before sending Enter.
        // Terminal injection is inherently unreliable - the terminal might be busy,
        // in a special state, or still processing previous input. 100ms is enough for
        // most terminals to settle without adding noticeable latency to multi-agent
        // conversations. If issues persist, consider adding retry logic or a more
        // reliable delivery mechanism (e.g., waiting for a terminal-ready signal).
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        pane.send_key("Enter")
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux send_key Enter failed: {}", e)))?;

        debug!(
            agent_id = key,
            bytes = message.len(),
            "Message injected via rmux"
        );
        Ok(())
    }

    async fn capture_output(&self, handle: &AgentHandle) -> ErgataiResult<Option<String>> {
        let key = handle.agent_id.clone();
        // Clone the Pane out, drop read guard before awaiting (same pattern as inject_message)
        let pane = {
            let panes = self.panes.read().await;
            panes.get(&key).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", key))
            })?
        };

        let captured = pane
            .screenshot()
            .await
            .map_err(|e| ErgataiError::internal(format!("rmux screenshot failed: {}", e)))?;

        Ok(Some(captured.text))
    }

    async fn is_alive(&self, handle: &AgentHandle) -> ErgataiResult<bool> {
        let key = handle.agent_id.clone();
        // Clone the pane handle out of the map, then drop the read guard
        // before making async calls to avoid blocking writers.
        let pane = {
            let panes = self.panes.read().await;
            match panes.get(&key) {
                Some(pane) => pane.clone(),
                None => return Ok(false),
            }
        };

        // Check if the pane's foreground process is still running.
        // `foreground_state()` returns:
        //   Ok(Some(_)) — a process is in the foreground → alive
        //   Ok(None)   — pane exists but no foreground process (e.g., at shell prompt)
        //   Err(_)     — pane has been closed → not alive
        match pane.foreground_state().await {
            Ok(Some(_)) => Ok(true),
            Ok(None) => {
                // Pane exists but no foreground — check pane slot liveness via title
                match pane.title().await {
                    Ok(_) => Ok(true),
                    Err(_) => Ok(false),
                }
            }
            Err(_) => Ok(false),
        }
    }

    async fn stop_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        let key = handle.agent_id.clone();
        info!(agent_id = key, "Stopping agent (closing rmux pane)");

        // Try local tracking first (fast path)
        let pane = self.panes.write().await.remove(&key);

        if let Some(pane) = pane {
            match pane.close().await {
                Ok(outcome) => {
                    debug!(agent_id = key, ?outcome, "Pane closed");
                }
                Err(e) => {
                    warn!(
                        agent_id = key,
                        error = %e,
                        "Failed to close pane (may already be closed)"
                    );
                }
            }
            return Ok(());
        }

        // Fallback: local tracking missed — try daemon-driven cleanup.
        // This handles the case where Ergatai restarted but the daemon still
        // has the session/pane, or the pane was never tracked locally.
        let session_name = handle
            .workspace
            .metadata
            .get("session")
            .cloned()
            .unwrap_or_else(|| self.session_name(&handle.workspace.id));

        warn!(
            agent_id = key,
            session = session_name,
            "Pane not in local tracking, falling back to daemon-driven cleanup"
        );

        // Find and close all running panes in this workspace's session
        let info = self.daemon_info().await;
        let mut closed_any = false;

        for mp in &info.managed_panes {
            if mp.session_name == session_name && mp.process_state == "running" {
                match self.stop_pane(&mp.session_name, &mp.pane_id).await {
                    Ok(()) => {
                        debug!(
                            agent_id = key,
                            pane_id = mp.pane_id,
                            "Daemon-driven pane closed during fallback"
                        );
                        closed_any = true;
                    }
                    Err(e) => {
                        warn!(
                            agent_id = key,
                            pane_id = mp.pane_id,
                            error = %e,
                            "Failed to close daemon-discovered pane during fallback"
                        );
                    }
                }
            }
        }

        if !closed_any {
            debug!(
                agent_id = key,
                session = session_name,
                "No running panes found in session (already cleaned up or never existed)"
            );
        }

        Ok(())
    }

    async fn kill_agent(&self, handle: &AgentHandle) -> ErgataiResult<()> {
        // For rmux, stop and kill are the same — close the pane (daemon sends SIGKILL)
        self.stop_agent(handle).await
    }

    async fn wait_for_exit(
        &self,
        handle: &AgentHandle,
        timeout: Option<Duration>,
    ) -> ErgataiResult<WaitResult> {
        let key = handle.agent_id.clone();
        let pane = {
            let panes = self.panes.read().await;
            panes.get(&key).cloned().ok_or_else(|| {
                ErgataiError::internal(format!("Pane not found for agent {}", key))
            })?
        };

        // Use daemon-level wait_for_exit (much better than polling):
        // The rmux daemon tracks pane process state and notifies us immediately
        // when the process exits, with actual exit code / signal.
        let wait_future = pane.wait_for_exit();

        let exit_state = if let Some(dur) = timeout {
            match tokio::time::timeout(dur, wait_future).await {
                Ok(Ok(state)) => state,
                Ok(Err(e)) => return Ok(WaitResult::Error(e.to_string())),
                Err(_) => return Ok(WaitResult::Timeout),
            }
        } else {
            match wait_future.await {
                Ok(state) => state,
                Err(e) => return Ok(WaitResult::Error(e.to_string())),
            }
        };

        // PaneExitState: { code: Option<i32>, signal: Option<i32>, message: Option<String> }
        // None means the pane was already stale or vanished before exit details were retained.
        match exit_state {
            Some(state) => {
                if let Some(signal) = state.signal {
                    debug!(agent_id = key, signal, "Agent exited via signal");
                    Ok(WaitResult::Signaled { signal })
                } else {
                    let code = state.code.unwrap_or(0);
                    debug!(agent_id = key, code, "Agent exited normally");
                    Ok(WaitResult::Exited { code })
                }
            }
            None => {
                debug!(agent_id = key, "Pane vanished (no exit state retained)");
                Ok(WaitResult::Exited { code: 0 })
            }
        }
    }

    async fn list_workspaces(&self) -> ErgataiResult<Vec<WorkspaceHandle>> {
        let rmux = match self.get_rmux().await {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };

        // Create a temporary session handle to list all sessions
        let probe_name = SessionName::new("__ergatai_list__")
            .map_err(|e| ErgataiError::internal(format!("Invalid probe name: {}", e)))?;
        let probe_session = match rmux
            .ensure_session(
                EnsureSession::named(probe_name)
                    .policy(EnsureSessionPolicy::CreateOrReuse)
                    .detached(true)
                    .size(TerminalSizeSpec::new(80, 24)),
            )
            .await
        {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };

        let session_names = match probe_session.list_session_names().await {
            Ok(names) => names,
            Err(_) => {
                let _ = probe_session.kill().await;
                return Ok(Vec::new());
            }
        };

        // Clean up probe session
        let _ = probe_session.kill().await;

        let prefix = format!("{}-", self.session_prefix);
        let work_dir_cache = self.work_dir_cache.read().await;
        let workspaces = session_names
            .into_iter()
            .filter(|name| name.as_str().starts_with(&prefix))
            .map(|name| {
                let name_str = name.as_str().to_string();
                let id = name_str
                    .strip_prefix(&prefix)
                    .unwrap_or(&name_str)
                    .to_string();
                let mut metadata = HashMap::new();
                metadata.insert("session".to_string(), name_str);
                // Include work_dir from cache so launch_agent can reuse it
                if let Some(work_dir) = work_dir_cache.get(&id) {
                    metadata.insert("work_dir".to_string(), work_dir.clone());
                }
                WorkspaceHandle {
                    id,
                    backend: "rmux".to_string(),
                    metadata,
                }
            })
            .collect();

        Ok(workspaces)
    }

    async fn cleanup_workspace(&self, handle: &WorkspaceHandle) -> ErgataiResult<()> {
        let rmux = self.get_rmux().await?;
        let session_name_str = handle
            .metadata
            .get("session")
            .cloned()
            .unwrap_or_else(|| self.session_name(&handle.id));

        info!(session = session_name_str, "Cleaning up rmux session");

        let session_name = SessionName::new(&session_name_str)
            .map_err(|e| ErgataiError::internal(format!("Invalid session name: {}", e)))?;

        let session = match rmux
            .ensure_session(
                EnsureSession::named(session_name)
                    .policy(EnsureSessionPolicy::CreateOrReuse)
                    .detached(true)
                    .size(TerminalSizeSpec::new(80, 24)),
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    session = session_name_str,
                    error = %e,
                    "Failed to get session handle for cleanup"
                );
                return Ok(());
            }
        };

        match session.kill().await {
            Ok(existed) => {
                debug!(
                    session = session_name_str,
                    existed = existed,
                    "Session killed"
                );
            }
            Err(e) => {
                warn!(
                    session = session_name_str,
                    error = %e,
                    "kill-session failed (may already be gone)"
                );
            }
        }

        // Clean up anchor pane reference
        self.anchor_panes.write().await.remove(&handle.id);

        Ok(())
    }

    async fn shutdown(&self) -> ErgataiResult<()> {
        let workspaces = self.list_workspaces().await?;
        info!(count = workspaces.len(), "Shutting down rmux backend");

        for ws in &workspaces {
            self.cleanup_workspace(ws).await?;
        }

        // Clear all pane references
        self.panes.write().await.clear();
        self.anchor_panes.write().await.clear();

        info!("rmux backend shutdown complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_simple() {
        assert_eq!(RmuxBackend::sanitize_message("hello world"), "hello world");
    }

    #[test]
    fn test_sanitize_strips_newlines() {
        let result = RmuxBackend::sanitize_message("line1\nline2\rline3");
        assert_eq!(result, "line1 line2 line3");
    }

    #[test]
    fn test_sanitize_truncates() {
        let big = "x".repeat(MAX_MESSAGE_SIZE + 100);
        let result = RmuxBackend::sanitize_message(&big);
        assert!(result.len() <= MAX_MESSAGE_SIZE + 20);
        assert!(result.ends_with("[truncated]"));
    }

    #[test]
    fn test_session_name() {
        let backend = RmuxBackend::new("ergatai");
        assert_eq!(backend.session_name("task-123"), "ergatai-task-123");
    }

    #[test]
    fn test_session_name_sanitizes() {
        let backend = RmuxBackend::new("ergatai");
        assert_eq!(backend.session_name("a|b:c.d"), "ergatai-a-b-c-d");
    }

    #[test]
    fn test_capabilities() {
        let backend = RmuxBackend::new("test");
        let caps = backend.capabilities();
        assert!(caps.supports_message_injection);
        assert!(caps.supports_output_capture);
        assert!(!caps.supports_resource_limits);
        assert!(caps.supports_workspace_reuse);
    }

    #[test]
    fn test_backend_name() {
        let backend = RmuxBackend::new("test");
        assert_eq!(backend.name(), "rmux");
    }

    #[test]
    fn test_with_dimensions() {
        let backend = RmuxBackend::new("test").with_dimensions(120, 40);
        assert_eq!(backend.width, 120);
        assert_eq!(backend.height, 40);
    }

    #[test]
    fn test_default_completion_markers_non_empty() {
        assert!(!DEFAULT_COMPLETION_MARKERS.is_empty());
        // All markers should be short, common strings
        for &marker in DEFAULT_COMPLETION_MARKERS {
            assert!(!marker.is_empty());
        }
    }

    #[test]
    fn test_text_wait_timeout_is_reasonable() {
        // 60s default — long enough for slow agents, short enough to fail fast
        assert_eq!(TEXT_WAIT_TIMEOUT, Duration::from_secs(60));
    }
}
