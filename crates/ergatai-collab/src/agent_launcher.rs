// Agent Launcher - Starts agents in isolated worktrees via ACP protocol
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
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use super::task_coordinator::{AgentAssignment, TaskCoordinator, TaskPlan};
use ergatai_acp::manager::{manager as session_manager, SessionCommand};
use ergatai_acp::agent_registry::agent_registry;
use ergatai_acp::http_client::http_connection_manager;

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
    /// ACP session ID (set after session creation succeeds)
    pub session_id: Option<String>,
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

/// Agent Launcher - manages agent ACP sessions
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
        let priority =
            ergatai_lock::conflict_arbitration::priority_to_number(&assignment.priority);
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
            session_id: Some(session_id.clone()),
            token_id: Some(file_token.id.to_string()),
        };

        self.running_agents
            .lock()
            .await
            .insert(agent_id.clone(), running_agent);

        // Launch ACP session — pass instruction text as prompt
        // Extract node_id from plan file (DAG nodes use {node_id}.md naming)
        let node_id = plan
            .plan_file
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        self.spawn_acp_session(
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

    /// Spawn an ACP session for the agent and send the instruction as a prompt.
    ///
    /// In middleware mode:
    /// 1. Look up agent's ACP endpoint from AgentRegistry
    /// 2. Connect via HttpClient
    /// 3. Send prompts via HTTP
    ///
    /// If `node_id` is Some, the agent is part of a DAG — completion/failure
    /// automatically triggers `DagScheduler::on_node_completed/failed`.
    async fn spawn_acp_session(
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
            "Spawning ACP session for agent via HTTP"
        );

        // 1. Get agent's ACP endpoint from registry
        let registry = agent_registry();
        let acp_endpoint = registry
            .get_acp_endpoint(agent_id)
            .await
            .ok_or_else(|| {
                ergatai_error::ErgataiError::AgentSpawnFailed(format!(
                    "Agent '{}' has no ACP endpoint registered. \
                     Agents must register their ACP endpoint via ergatai.set_acp_endpoint tool.",
                    agent_id
                ))
            })?;

        tracing::info!(
            agent = %agent_id,
            endpoint = %acp_endpoint,
            "Found ACP endpoint for agent"
        );

        // 2. Connect via HttpConnectionManager
        let http_manager = http_connection_manager();
        let cwd = worktree_path.to_string_lossy().to_string();

        // Check if already connected
        let session_id = if http_manager.is_connected(agent_id).await {
            tracing::info!(
                agent = %agent_id,
                "Reusing existing HTTP connection to agent"
            );
            // TODO: Enhance HttpConnectionManager to return existing session_id
            // For now, we must create a new connection for each DAG task
            // because session state is task-specific
            http_manager
                .connect(agent_id, &acp_endpoint, cwd, ergatai_acp::manager::SessionKind::Dag)
                .await
                .map_err(|e| {
                    ergatai_error::ErgataiError::AgentSpawnFailed(format!(
                        "Failed to reconnect to agent '{}': {}",
                        agent_id, e
                    ))
                })?
        } else {
            tracing::info!(
                agent = %agent_id,
                "Creating new HTTP connection to agent"
            );
            http_manager
                .connect(agent_id, &acp_endpoint, cwd, ergatai_acp::manager::SessionKind::Dag)
                .await
                .map_err(|e| {
                    ergatai_error::ErgataiError::AgentSpawnFailed(format!(
                        "Failed to connect to agent '{}': {}",
                        agent_id, e
                    ))
                })?
        };

        tracing::info!(
            agent = %agent_id,
            session_id = %session_id,
            "HTTP ACP session created"
        );

        // 3. Update RunningAgent with session_id
        {
            let mut agents = self.running_agents.lock().await;
            if let Some(agent) = agents.get_mut(agent_id) {
                agent.status = AgentStatus::Running;
                agent.session_id = Some(session_id.clone());
            }
        }

        // 4. Send instruction as prompt
        tracing::info!(
            agent = %agent_id,
            instruction_len = instruction.len(),
            "Sending instruction to agent"
        );

        http_manager
            .send_prompt(agent_id, instruction.to_string())
            .await
            .map_err(|e| {
                ergatai_error::ErgataiError::AgentSpawnFailed(format!(
                    "Failed to send prompt to agent '{}': {}",
                    agent_id, e
                ))
            })?;

        tracing::info!(
            agent = %agent_id,
            "Instruction sent to agent successfully"
        );

        // 5. Monitor for completion (in background)
        // TODO: Implement completion monitoring via SessionNotification
        // For now, we'll assume the agent completes successfully
        // In a real implementation, we would:
        // - Listen for SessionNotification from the agent
        // - Update agent status based on notifications
        // - Trigger DagScheduler::on_node_completed/failed if node_id is Some

        if let Some(node_id) = node_id {
            tracing::info!(
                agent = %agent_id,
                node_id = %node_id,
                "Agent is part of DAG, will notify DagScheduler on completion"
            );
            // TODO: Set up completion notification to DagScheduler
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

    /// Clean up agent resources (ACP session + worktree + tokens)
    pub async fn cleanup_agent(&self, agent_id: &str) -> ErgataiResult<()> {
        if let Some(agent) = self.running_agents.lock().await.remove(agent_id) {
            // Close ACP session if still active
            if let Some(ref session_id) = agent.session_id {
                if let Some(cmd_tx) = session_manager().get_cmd_tx(session_id).await {
                    let _ = cmd_tx.send(SessionCommand::Close);
                }

                // Clear watchdog busy status
                if let Ok(watchdog) = ergatai_lock::get_watchdog(&agent.task_id).await {
                    let watchdog = watchdog.write().await;
                    let _ = watchdog.clear_busy(session_id).await;
                }
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
        // Collect stale agent info while holding lock, then release lock before sending Close
        let stale_sessions: Vec<(String, String)> = {
            let mut agents = self.running_agents.lock().await;
            let stale: Vec<String> = agents
                .iter()
                .filter(|(_, a)| {
                    a.status == AgentStatus::Completed || a.status == AgentStatus::Failed
                })
                .map(|(id, _)| id.clone())
                .collect();

            let mut sessions = Vec::with_capacity(stale.len());
            for id in &stale {
                if let Some(agent) = agents.remove(id) {
                    // Collect session IDs for closing after releasing lock
                    if let Some(session_id) = agent.session_id {
                        sessions.push((id.clone(), session_id));
                    }
                }
            }

            if !stale.is_empty() {
                tracing::info!(count = stale.len(), "Cleared stale agents from registry");
            }

            sessions
            // Lock released here
        };

        // Now send Close commands and revoke tokens without holding the lock
        for (agent_id, session_id) in &stale_sessions {
            if let Some(cmd_tx) = session_manager().get_cmd_tx(session_id).await {
                if let Err(e) = cmd_tx.send(SessionCommand::Close) {
                    tracing::warn!(
                        agent_id = %agent_id,
                        session_id = %session_id,
                        error = %e,
                        "Failed to close lingering ACP session for stale agent"
                    );
                }
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
