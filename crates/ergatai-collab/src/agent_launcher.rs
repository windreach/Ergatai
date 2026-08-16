// Agent Launcher - Starts agents in tmux panes and injects tasks
// Manages agent sessions and monitors their completion
//
// ARCHITECTURE NOTE (Phase 8 - File Access Control Integration):
// This module is being migrated from git worktree isolation to file access control.
// Current state: Hybrid mode (worktrees + file access tokens)
// Target state: File access control only (worktrees removed)
//
// Migration progress:
// - ✅ File access control initialization
// - ✅ System Token registration for each agent
// - ✅ File Token request based on task scope
// - ⏳ Remove worktree creation (pending testing)
// - ⏳ Update agent instructions to use project root (pending testing)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ergatai_error::ErgataiResult;
use ergatai_lock::{FileMode, FileToken, SystemToken};
use ergatai_runtime::{get_agent_runtime, WorkspaceSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use super::task_coordinator::{AgentAssignment, TaskCoordinator, TaskPlan};

/// Agent session status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentStatus {
    Starting,
    Running,
    Completed,
    Failed,
}

/// Running agent information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningAgent {
    pub task_id: String,
    pub agent_name: String,
    pub worktree_path: PathBuf,
    pub plan_file: PathBuf,
    pub result_file: PathBuf,
    pub status: AgentStatus,
    /// Tmux pane ID where the agent is running (e.g. "ergatai-opencode:0.0")
    pub pane_id: Option<String>,
    /// File access token ID (for file access control)
    pub token_id: Option<String>,
}

/// Global registry of running agents.
///
/// `AgentLauncher` is constructed per-NAPI-call, but we need running-agent
/// state to persist across calls so that `task_get_agents_status`,
/// `task_all_agents_completed`, and `task_merge_all` can observe agents
/// launched by prior calls. A global `OnceLock`-backed map gives us that
/// without threading handles through the NAPI layer.
fn running_agents() -> Arc<Mutex<HashMap<String, RunningAgent>>> {
    static REGISTRY: OnceLock<Arc<Mutex<HashMap<String, RunningAgent>>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Safely truncate a UTF-8 string to at most max_len bytes,
/// ensuring we don't split multi-byte characters.
fn safe_truncate_utf8(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }

    // Find the largest character boundary <= max_len
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}\n\n[... truncated ...]", &s[..end])
}

/// Agent Launcher - manages agent tmux sessions
pub struct AgentLauncher {
    coordinator: Arc<TaskCoordinator>,
    running_agents: Arc<Mutex<HashMap<String, RunningAgent>>>,
}

impl AgentLauncher {
    /// Create a new AgentLauncher
    pub fn new(project_root: PathBuf) -> Self {
        let coordinator = Arc::new(TaskCoordinator::new(project_root));
        let running_agents = running_agents();

        Self {
            coordinator,
            running_agents,
        }
    }

    /// Launch all agents for a task plan
    pub async fn launch_agents(&self, plan: &TaskPlan) -> ErgataiResult<Vec<String>> {
        let mut agent_ids = Vec::with_capacity(plan.assignments.len());

        for assignment in &plan.assignments {
            let agent_id = self.launch_agent(plan, assignment).await?;
            agent_ids.push(agent_id);
        }

        Ok(agent_ids)
    }

    /// Launch a single agent for a task assignment
    pub async fn launch_agent(
        &self,
        plan: &TaskPlan,
        assignment: &AgentAssignment,
    ) -> ErgataiResult<String> {
        let agent_id = Self::make_agent_id(&plan.task_id, &assignment.agent_name);
        let project_id = &plan.task_id;
        let project_root = self.coordinator.project_root.clone();

        // Initialize file access control for the project (idempotent)
        ergatai_lock::init_file_access(project_id, &project_root).await?;

        // Get FileLockManager
        let lock_manager = ergatai_lock::get_lock_manager(project_id).await?;

        // Register System Token for this agent
        let session_id = format!("session-{}", agent_id);
        let system_token = SystemToken::new(
            assignment.agent_name.clone(),
            session_id.clone(),
            project_root.to_string_lossy().to_string(),
            3600, // 1 hour TTL
            30,   // 30 second heartbeat
        );

        info!(
            agent_id = agent_id,
            token_id = %system_token.id,
            "Registering system token for agent"
        );

        // Actually register the token
        lock_manager.register_system_token(&system_token)?;

        // Determine file scope based on assignment
        let scope =
            if assignment.files_to_modify.is_empty() && assignment.files_to_create.is_empty() {
                "**".to_string() // Full project access when no files specified
            } else {
                // Build scope from file list
                let mut patterns = Vec::with_capacity(
                    assignment.files_to_create.len() + assignment.files_to_modify.len(),
                );
                patterns.extend(
                    assignment
                        .files_to_create
                        .iter()
                        .map(|p| p.to_string_lossy().to_string()),
                );
                patterns.extend(
                    assignment
                        .files_to_modify
                        .iter()
                        .map(|p| p.to_string_lossy().to_string()),
                );

                if patterns.is_empty() {
                    "**".to_string()
                } else {
                    patterns.join(",")
                }
            };

        // Request File Token
        let priority = ergatai_lock::conflict_arbitration::priority_to_number(&assignment.priority);
        let file_token = FileToken::with_priority(
            assignment.agent_name.clone(),
            session_id.clone(),
            system_token.id.clone(),
            scope.clone(),
            FileMode::Write, // Agents need write access
            Some(format!("Task: {}", assignment.objective)),
            "system".to_string(), // System auto-approves for now
            3600,                 // 1 hour TTL
            30,                   // 30 second heartbeat
            priority,
        );

        info!(
            agent_id = agent_id,
            token_id = %file_token.id,
            scope = scope,
            "File token granted"
        );

        // Start Watchdog heartbeat for this session
        let watchdog = ergatai_lock::get_watchdog(project_id).await?;
        {
            let watchdog = watchdog.write().await;
            watchdog.mark_busy(&session_id, 3600).await?;
        }

        // Use project root (no worktree)
        let work_dir = project_root.clone();

        // Get result file path
        let result_file = self
            .coordinator
            .get_result_path(&plan.task_id, &assignment.agent_name)?;

        // Copy AGENT.md to work_dir (if exists)
        let agent_guide_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
        if tokio::fs::try_exists(&agent_guide_path)
            .await
            .unwrap_or(false)
        {
            let workdir_agent_guide = work_dir.join("AGENT.md");
            tokio::fs::copy(&agent_guide_path, &workdir_agent_guide).await?;
        }

        // Create agent instruction text
        let instruction = self
            .create_agent_instruction(
                &assignment.agent_name,
                &work_dir,
                &plan.plan_file,
                &result_file,
                assignment,
            )
            .await;

        // Save instruction to file for debugging/auditing
        let instruction_file = work_dir.join(format!(".ergatai-task-{}.md", agent_id));
        tokio::fs::write(&instruction_file, &instruction).await?;

        // Create running agent record
        let running_agent = RunningAgent {
            task_id: plan.task_id.clone(),
            agent_name: assignment.agent_name.clone(),
            worktree_path: work_dir.clone(),
            plan_file: plan.plan_file.clone(),
            result_file: result_file.clone(),
            status: AgentStatus::Starting,
            pane_id: None,
            token_id: Some(file_token.id.to_string()),
        };

        self.running_agents
            .lock()
            .await
            .insert(agent_id.clone(), running_agent);

        // Launch agent in tmux pane — pass instruction text as injected prompt
        // Extract node_id from plan file (DAG nodes use {node_id}.md naming)
        let node_id = plan
            .plan_file
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        self.spawn_tmux_session(
            &agent_id,
            &work_dir,
            &assignment.agent_name,
            &instruction,
            node_id,
        )
        .await?;

        Ok(agent_id)
    }

    /// Build a deterministic agent id from task_id + agent_name.
    ///
    /// Uses `|` as separator because both task_id and agent_name may contain `-`.
    pub fn make_agent_id(task_id: &str, agent_name: &str) -> String {
        format!("{}|{}", task_id, agent_name)
    }

    /// Parse a (task_id, agent_name) pair from an agent id produced by `make_agent_id`.
    pub fn parse_agent_id(agent_id: &str) -> Option<(&str, &str)> {
        agent_id.split_once('|')
    }

    /// Create instruction for agent (in English for token efficiency)
    async fn create_agent_instruction(
        &self,
        agent_name: &str,
        work_dir: &Path,
        plan_file: &Path,
        result_file: &Path,
        assignment: &AgentAssignment,
    ) -> String {
        let task_type_dbg = format!("{:?}", assignment.task_type);
        let files_section = self.format_files_section(assignment);

        // Read project context
        let project_context = self.read_project_context().await;

        format!(
            r#"# Project Context

{project_context}

---

# Task Assignment for @{agent_name}

## Your Work Directory
```
{work_dir}
```

You are working in the project root directory with file access control enabled.
The system manages file locks to prevent conflicts with other agents.

## Task Plan
Read the full plan: `{plan_file}`

Find your assignment section (marked with `@{agent_name}`)

## Your Objective
{objective}

## Task Type
{task_type}

## Files
{files_section}

## Instructions

1. Read the plan file to understand the full context
2. Work in the project directory (file access control is active)
3. Complete your assigned task
4. Write your results to: `{result_file}`

## Result Format

Write your results in markdown:

```markdown
# Task Result

## Status
[Completed/Failed/Partial]

## Summary
[Brief summary of what you did]

## Details
[Detailed description of your work]

## Files Created/Modified
- [list of files]

## Notes
[Any additional notes or issues]
```

## Important Notes

- File access control is active - the system manages file locks automatically
- Focus only on your assigned objective
- If you encounter issues, document them in your result file
- Complete your task and write the result file when done
"#,
            project_context = project_context,
            agent_name = agent_name,
            work_dir = work_dir.display(),
            plan_file = plan_file.display(),
            objective = assignment.objective,
            task_type = task_type_dbg,
            files_section = files_section,
            result_file = result_file.display(),
        )
    }

    /// Format files section for instruction
    fn format_files_section(&self, assignment: &AgentAssignment) -> String {
        // Pre-allocate for both sections
        let mut sections = Vec::with_capacity(2);

        if !assignment.files_to_create.is_empty() {
            sections.push("**Files to create:**".to_string());
            for file in &assignment.files_to_create {
                sections.push(format!("- {}", file.display()));
            }
        }

        if !assignment.files_to_modify.is_empty() {
            sections.push("**Files to modify:**".to_string());
            for file in &assignment.files_to_modify {
                sections.push(format!("- {}", file.display()));
            }
        }

        if !assignment.files_to_read.is_empty() {
            sections.push("**Files to read:**".to_string());
            for file in &assignment.files_to_read {
                sections.push(format!("- {}", file.display()));
            }
        }

        if sections.is_empty() {
            "No specific files assigned".to_string()
        } else {
            sections.join("\n")
        }
    }

    /// Spawn an agent using AgentRuntime and inject the instruction as a prompt.
    ///
    /// Steps:
    /// 1. Get global AgentRuntime singleton (fixes lifetime bug)
    /// 2. Create workspace spec with work_dir and backend config
    /// 3. Launch agent via runtime (creates workspace + starts process)
    /// 4. Inject instruction via runtime (backend injection or MCP fallback)
    /// 5. Spawn a background watcher that monitors agent exit and
    ///    publishes NATS events so DagScheduler picks up completion/failure.
    ///
    /// If `node_id` is Some, the agent is part of a DAG — completion/failure
    /// automatically triggers `DagScheduler::on_node_completed/failed`.
    async fn spawn_tmux_session(
        &self,
        agent_id: &str,
        worktree_path: &Path,
        agent_name: &str,
        instruction: &str,
        node_id: Option<String>,
    ) -> ErgataiResult<()> {
        tracing::info!(
            agent = %agent_id,
            agent_name = %agent_name,
            worktree = %worktree_path.display(),
            "Spawning agent via AgentRuntime"
        );

        // 1. Get global AgentRuntime singleton (fixes lifetime bug)
        let runtime = get_agent_runtime();

        // 2. Create workspace spec
        let workspace_id = format!("dag-{}", agent_id.replace('|', "-"));
        let spec = WorkspaceSpec {
            id: workspace_id.clone(),
            work_dir: worktree_path.to_path_buf(),
            env: std::collections::HashMap::new(),
            resources: Default::default(),
            backend_config: serde_json::json!({}),
        };

        // 3. Build the agent launch command.
        //    Default: `claude` (Claude Code CLI). Users can override by setting
        //    ERGATAI_AGENT_CMD env var.
        let agent_command =
            std::env::var("ERGATAI_AGENT_CMD").unwrap_or_else(|_| "claude".to_string());

        // Validate agent command — reject obvious injection patterns
        if agent_command.is_empty() {
            return Err(ergatai_error::ErgataiError::AgentSpawnFailed(
                "ERGATAI_AGENT_CMD is empty".to_string(),
            ));
        }
        if agent_command.contains('\n') || agent_command.contains('\r') {
            return Err(ergatai_error::ErgataiError::AgentSpawnFailed(
                "ERGATAI_AGENT_CMD contains newline characters".to_string(),
            ));
        }

        // 4. Launch agent via runtime (creates workspace + starts process)
        let runtime_agent_id = runtime
            .launch_agent(spec, &agent_command, Some(instruction))
            .await
            .map_err(|e| {
                ergatai_error::ErgataiError::AgentSpawnFailed(format!(
                    "Failed to launch agent '{}': {}",
                    agent_id, e
                ))
            })?;

        // 5. Set task_id and mcp_agent_id for tracking
        if let Err(e) = runtime
            .set_task_id(&runtime_agent_id, agent_id.to_string())
            .await
        {
            tracing::warn!(
                agent = %agent_id,
                runtime_agent_id = %runtime_agent_id,
                error = %e,
                "Failed to set task_id on runtime — DAG completion tracking may be broken"
            );
        }

        // Set agent name as MCP agent ID for notification routing
        if let Err(e) = runtime
            .set_mcp_agent_id(&runtime_agent_id, agent_name.to_string())
            .await
        {
            tracing::warn!(
                agent = %agent_id,
                runtime_agent_id = %runtime_agent_id,
                error = %e,
                "Failed to set mcp_agent_id on runtime — MCP notification fallback may not route"
            );
        }

        tracing::info!(
            agent = %agent_id,
            runtime_agent_id = %runtime_agent_id,
            "Agent launched via AgentRuntime"
        );

        // 6. Update RunningAgent with status
        {
            let mut agents = self.running_agents.lock().await;
            if let Some(agent) = agents.get_mut(agent_id) {
                agent.status = AgentStatus::Running;
            }
        }

        // 7. Background watcher — monitor agent exit, then publish NATS event.
        if let Some(node_id_val) = node_id {
            let agent_id_monitor = agent_id.to_string();
            let agent_name_monitor = agent_name.to_string();
            let node_id_monitor = node_id_val.clone();
            let running_agents = self.running_agents.clone();
            let runtime_monitor = runtime.clone();
            let runtime_agent_id_monitor = runtime_agent_id.clone();

            tracing::info!(
                agent = %agent_id,
                node_id = %node_id_val,
                "Spawning agent-exit watcher for DAG agent"
            );

            tokio::spawn(async move {
                // Use runtime.wait_for_exit() to monitor agent
                let max_runtime = std::time::Duration::from_secs(3600); // 1h hard cap

                let wait_result = tokio::select! {
                    result = runtime_monitor.wait_for_exit(&runtime_agent_id_monitor, Some(max_runtime)) => {
                        result
                    }
                    _ = tokio::time::sleep(max_runtime) => {
                        tracing::warn!(
                            agent = %agent_id_monitor,
                            "Agent exceeded max runtime, marking as failed"
                        );
                        let _ = runtime_monitor.stop_agent(&runtime_agent_id_monitor).await;
                        Ok(ergatai_runtime::WaitResult::Timeout)
                    }
                };

                match wait_result {
                    Ok(ergatai_runtime::WaitResult::Exited { code: _ }) => {
                        tracing::info!(
                            agent = %agent_id_monitor,
                            node_id = %node_id_monitor,
                            "Agent exited normally"
                        );
                    }
                    Ok(ergatai_runtime::WaitResult::Signaled { signal }) => {
                        tracing::warn!(
                            agent = %agent_id_monitor,
                            signal = signal,
                            "Agent killed by signal"
                        );
                    }
                    Ok(ergatai_runtime::WaitResult::Timeout) => {
                        tracing::warn!(
                            agent = %agent_id_monitor,
                            "Agent wait timed out"
                        );
                    }
                    Ok(ergatai_runtime::WaitResult::Error(e)) => {
                        tracing::error!(
                            agent = %agent_id_monitor,
                            error = %e,
                            "Agent wait error"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            agent = %agent_id_monitor,
                            error = %e,
                            "Agent wait failed"
                        );
                    }
                }

                // Determine success/failure heuristically:
                //   - result file exists → Completed
                //   - otherwise → Failed
                let result_file_path = {
                    let agents = running_agents.lock().await;
                    agents.get(&agent_id_monitor).map(|a| a.result_file.clone())
                };

                let result_file_exists = if let Some(ref path) = result_file_path {
                    tokio::fs::try_exists(path).await.unwrap_or(false)
                } else {
                    false
                };

                let (status, error_msg) = if result_file_exists {
                    (AgentStatus::Completed, None)
                } else {
                    (
                        AgentStatus::Failed,
                        Some(format!(
                            "Agent exited without producing result file: {}",
                            result_file_path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "(unknown)".into())
                        )),
                    )
                };

                // Update RunningAgent status
                {
                    let mut agents = running_agents.lock().await;
                    if let Some(agent) = agents.get_mut(&agent_id_monitor) {
                        agent.status = status.clone();
                    }
                }

                // Capture output for result summary (best-effort)
                let result_summary = runtime_monitor
                    .capture_output(&runtime_agent_id_monitor)
                    .await
                    .ok()
                    .flatten()
                    .map(|s| {
                        if s.len() > 2000 {
                            s[s.len() - 2000..].to_string()
                        } else {
                            s
                        }
                    });

                // Publish NATS event for DagScheduler
                if ergatai_nats::is_nats_initialized().await {
                    if let Some(conn) = ergatai_nats::get_nats_connection().await {
                        let bus = ergatai_nats::event_bus::EventBus::new(conn);
                        match &result_file_exists {
                            true => {
                                let payload = ergatai_nats::events::NodeCompletePayload {
                                    node_id: node_id_monitor.clone(),
                                    task_id: node_id_monitor.clone(),
                                    agent_name: agent_name_monitor.clone(),
                                    result_summary,
                                    outputs: HashMap::new(),
                                    result_file: result_file_path
                                        .map(|p| p.to_string_lossy().to_string()),
                                };
                                if let Err(e) = bus.publish_node_complete(&payload).await {
                                    tracing::error!(
                                        error = %e,
                                        node_id = %node_id_monitor,
                                        "Failed to publish node_complete"
                                    );
                                }
                            }
                            false => {
                                let payload = ergatai_nats::events::NodeFailedPayload {
                                    node_id: node_id_monitor.clone(),
                                    task_id: node_id_monitor.clone(),
                                    agent_name: agent_name_monitor.clone(),
                                    error: error_msg.unwrap_or_default(),
                                    retryable: false,
                                };
                                if let Err(e) = bus.publish_node_failed(&payload).await {
                                    tracing::error!(
                                        error = %e,
                                        node_id = %node_id_monitor,
                                        "Failed to publish node_failed"
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        Ok(())
    }

    /// Get status of all running agents
    pub async fn get_all_status(&self) -> Vec<RunningAgent> {
        self.running_agents.lock().await.values().cloned().collect()
    }

    /// Get status of specific agent
    pub async fn get_agent_status(&self, agent_id: &str) -> Option<RunningAgent> {
        self.running_agents.lock().await.get(agent_id).cloned()
    }

    /// Check if all agents for a task are completed
    pub async fn all_agents_completed(&self, task_id: &str) -> bool {
        let agents = self.running_agents.lock().await;
        let mut any = false;
        for a in agents.values() {
            if a.task_id != task_id {
                continue;
            }
            any = true;
            if a.status != AgentStatus::Completed && a.status != AgentStatus::Failed {
                return false;
            }
        }
        any
    }

    /// Clean up agent resources (runtime agent + file tokens)
    pub async fn cleanup_agent(&self, agent_id: &str) -> ErgataiResult<()> {
        if let Some(agent) = self.running_agents.lock().await.remove(agent_id) {
            // Stop the agent via runtime if still running
            let runtime = get_agent_runtime();
            // Find the runtime agent ID by task_id
            let runtime_agent_id = runtime
                .list_agents()
                .await
                .iter()
                .find(|info| info.task_id.as_deref() == Some(agent_id))
                .map(|info| info.agent_id.clone());

            if let Some(runtime_id) = runtime_agent_id {
                if let Err(e) = runtime.stop_agent(&runtime_id).await {
                    tracing::debug!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to stop agent via runtime (may have already exited)"
                    );
                }
            }

            // Clear watchdog busy status (keyed by logical session id)
            let session_id = format!("session-{}", agent_id);
            if let Ok(watchdog) = ergatai_lock::get_watchdog(&agent.task_id).await {
                let watchdog = watchdog.write().await;
                let _ = watchdog.clear_busy(&session_id).await;
            }

            // SECURITY: Revoke file access tokens so completed/failed agents
            // don't retain file write permissions beyond their lifetime.
            if let Some(ref token_id) = agent.token_id {
                match ergatai_lock::get_lock_manager(&agent.task_id).await {
                    Ok(lock_manager) => {
                        if let Err(e) = lock_manager.expire_token(token_id) {
                            tracing::warn!(
                                agent_id = %agent_id,
                                token_id = %token_id,
                                error = %e,
                                "Failed to expire file access token during cleanup"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            agent_id = %agent_id,
                            error = %e,
                            "Lock manager not available for token revocation (may not be initialized)"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Remove completed/failed agents from tracking.
    ///
    /// Called at the start of each new DAG run to prevent ghost agents
    /// from previous runs appearing in `get_all_status()` results.
    pub async fn clear_stale_agents(&self) -> ErgataiResult<()> {
        let stale_agents: Vec<(String, String)> = {
            let mut agents = self.running_agents.lock().await;
            let stale: Vec<_> = agents
                .iter()
                .filter(|(_, a)| {
                    a.status == AgentStatus::Completed || a.status == AgentStatus::Failed
                })
                .map(|(id, a)| (id.clone(), a.task_id.clone()))
                .collect();

            let mut result = Vec::with_capacity(stale.len());
            for (id, task_id) in stale {
                if agents.remove(&id).is_some() {
                    result.push((id, task_id));
                }
            }

            if !result.is_empty() {
                tracing::info!(count = result.len(), "Cleared stale agents from registry");
            }

            result
            // Lock released here
        };

        // Now clean up agents + tokens without holding the lock
        let runtime = get_agent_runtime();
        for (agent_id, task_id) in &stale_agents {
            // Find and stop via runtime
            let runtime_agent_id = runtime
                .list_agents()
                .await
                .iter()
                .find(|info| info.task_id.as_deref() == Some(agent_id.as_str()))
                .map(|info| info.agent_id.clone());

            if let Some(runtime_id) = runtime_agent_id {
                if let Err(e) = runtime.stop_agent(&runtime_id).await {
                    tracing::debug!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to stop stale agent via runtime"
                    );
                }
            }

            let session_id = format!("session-{}", agent_id);
            if let Ok(watchdog) = ergatai_lock::get_watchdog(task_id).await {
                let watchdog = watchdog.write().await;
                let _ = watchdog.clear_busy(&session_id).await;
            }
        }

        Ok(())
    }

    /// Read project context from .ergatai/AGENT.md
    /// Returns default message if file doesn't exist
    /// Truncates to 10KB if file is too large
    pub(crate) async fn read_project_context(&self) -> String {
        let agent_md_path = self.coordinator.project_root.join(".ergatai/AGENT.md");

        match tokio::fs::read_to_string(&agent_md_path).await {
            Ok(content) => {
                const MAX_SIZE: usize = 10_000; // 10KB

                if content.len() > MAX_SIZE {
                    tracing::warn!(
                        "AGENT.md too large ({} bytes), truncating to {} bytes",
                        content.len(),
                        MAX_SIZE
                    );
                    safe_truncate_utf8(&content, MAX_SIZE)
                } else {
                    content
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("AGENT.md not found, using default context");
                "No project context provided.".to_string()
            }
            Err(e) => {
                tracing::warn!("Failed to read AGENT.md: {}", e);
                "No project context provided.".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_coordinator::TaskType;

    #[test]
    fn test_make_and_parse_agent_id() {
        // task_id with dashes should round-trip correctly
        let id = AgentLauncher::make_agent_id("task-001-alpha", "claude-code");
        assert_eq!(id, "task-001-alpha|claude-code");
        let parsed = AgentLauncher::parse_agent_id(&id).expect("should parse");
        assert_eq!(parsed, ("task-001-alpha", "claude-code"));
    }

    #[test]
    fn test_parse_agent_id_invalid() {
        // No separator → None
        assert_eq!(AgentLauncher::parse_agent_id("noseparator"), None);
    }

    #[test]
    fn test_all_agents_completed_empty() {
        // No agents for the task → should return false (no agent has completed)
        let launcher = AgentLauncher::new(std::env::temp_dir());
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(!rt.block_on(launcher.all_agents_completed("nonexistent")));
    }

    #[test]
    fn test_safe_truncate_utf8_ascii() {
        let content = "a".repeat(10001);
        let truncated = safe_truncate_utf8(&content, 10000);
        assert_eq!(truncated.len(), 10000 + "\n\n[... truncated ...]".len());
        assert!(truncated.ends_with("[... truncated ...]"));
    }

    #[test]
    fn test_safe_truncate_utf8_multibyte() {
        // 每个中文字符 3 字节
        let content = "中".repeat(3334); // 10002 字节
        let truncated = safe_truncate_utf8(&content, 10000);

        // 应该小于等于 10000 + 后缀长度
        assert!(truncated.len() <= 10000 + "\n\n[... truncated ...]".len());
        // 应该在字符边界截断
        let truncated_content = &truncated[..truncated.len() - "\n\n[... truncated ...]".len()];
        assert!(truncated_content.is_char_boundary(truncated_content.len()));
        // 不应该 panic
        assert!(truncated_content.chars().count() > 0);
    }

    #[test]
    fn test_safe_truncate_utf8_no_truncation_needed() {
        let content = "short content";
        let truncated = safe_truncate_utf8(content, 10000);
        assert_eq!(truncated, content);
    }

    #[tokio::test]
    async fn test_read_project_context_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create .ergatai directory and AGENT.md
        let ergatai_dir = project_root.join(".ergatai");
        tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
        tokio::fs::write(ergatai_dir.join("AGENT.md"), "test context")
            .await
            .unwrap();

        let launcher = AgentLauncher::new(project_root);
        let context = launcher.read_project_context().await;

        assert_eq!(context, "test context");
    }

    #[tokio::test]
    async fn test_read_project_context_not_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        let launcher = AgentLauncher::new(project_root);
        let context = launcher.read_project_context().await;

        assert_eq!(context, "No project context provided.");
    }

    #[tokio::test]
    async fn test_read_project_context_truncation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create .ergatai directory and large AGENT.md
        let ergatai_dir = project_root.join(".ergatai");
        tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
        let large_content = "a".repeat(15000);
        tokio::fs::write(ergatai_dir.join("AGENT.md"), &large_content)
            .await
            .unwrap();

        let launcher = AgentLauncher::new(project_root);
        let context = launcher.read_project_context().await;

        // Should be truncated
        assert!(context.len() < 15000);
        assert!(context.ends_with("[... truncated ...]"));
    }

    #[tokio::test]
    async fn test_create_agent_instruction_includes_context() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create .ergatai directory and AGENT.md
        let ergatai_dir = project_root.join(".ergatai");
        tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();
        tokio::fs::write(
            ergatai_dir.join("AGENT.md"),
            "# My Project Context\n\nThis is important.",
        )
        .await
        .unwrap();

        let launcher = AgentLauncher::new(project_root.clone());

        let assignment = AgentAssignment {
            agent_name: "test-agent".to_string(),
            objective: "Test objective".to_string(),
            files_to_create: vec![],
            files_to_modify: vec![],
            files_to_read: vec![],
            task_type: TaskType::CreateNew,
            depends_on: vec![],
            priority: None,
        };

        let worktree_path = project_root.join("worktree");
        let plan_file = project_root.join("plan.md");
        let result_file = project_root.join("result.md");

        let instruction = launcher
            .create_agent_instruction(
                "test-agent",
                &worktree_path,
                &plan_file,
                &result_file,
                &assignment,
            )
            .await;

        // Should include project context
        assert!(instruction.contains("# My Project Context"));
        assert!(instruction.contains("This is important."));
        // Should include task assignment
        assert!(instruction.contains("@test-agent"));
        assert!(instruction.contains("Test objective"));
    }

    #[tokio::test]
    async fn test_create_agent_instruction_no_agent_md_uses_default() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();
        // No .ergatai/AGENT.md created

        let launcher = AgentLauncher::new(project_root.clone());

        let assignment = AgentAssignment {
            agent_name: "test-agent".to_string(),
            objective: "Test".to_string(),
            files_to_create: vec![],
            files_to_modify: vec![],
            files_to_read: vec![],
            task_type: TaskType::CreateNew,
            depends_on: vec![],
            priority: None,
        };

        let instruction = launcher
            .create_agent_instruction(
                "test-agent",
                &project_root.join("worktree"),
                &project_root.join("plan.md"),
                &project_root.join("result.md"),
                &assignment,
            )
            .await;

        assert!(
            instruction.contains("No project context provided."),
            "instruction should contain default message when AGENT.md is missing"
        );
    }

    #[tokio::test]
    async fn test_create_agent_instruction_large_file_truncated() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Create .ergatai/AGENT.md with content exceeding 10KB
        let ergatai_dir = project_root.join(".ergatai");
        tokio::fs::create_dir_all(&ergatai_dir).await.unwrap();

        let large_content = format!("{}END_MARKER", "A".repeat(11_000));
        tokio::fs::write(ergatai_dir.join("AGENT.md"), &large_content)
            .await
            .unwrap();

        let launcher = AgentLauncher::new(project_root.clone());

        let assignment = AgentAssignment {
            agent_name: "test-agent".to_string(),
            objective: "Test truncation".to_string(),
            files_to_create: vec![],
            files_to_modify: vec![],
            files_to_read: vec![],
            task_type: TaskType::CreateNew,
            depends_on: vec![],
            priority: None,
        };

        let instruction = launcher
            .create_agent_instruction(
                "test-agent",
                &project_root.join("worktree"),
                &project_root.join("plan.md"),
                &project_root.join("result.md"),
                &assignment,
            )
            .await;

        // Content beyond 10KB should not appear
        assert!(
            !instruction.contains("END_MARKER"),
            "truncated instruction should not contain content beyond 10KB"
        );
        // Truncation marker should be present
        assert!(
            instruction.contains("[... truncated ...]"),
            "instruction should contain truncation marker"
        );
    }
}
