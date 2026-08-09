// Agent Launcher - Starts agents in isolated worktrees via ACP protocol
// Manages agent sessions and monitors their completion

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Context;
use crate::error::ErgataiResult;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};

use super::task_coordinator::{AgentAssignment, TaskCoordinator, TaskPlan};
use crate::acp::manager::{manager as session_manager, SessionCommand, SessionKind};

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
        let mut agent_ids = Vec::new();

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

        // Create worktree
        let worktree_path = self
            .coordinator
            .create_worktree(&plan.task_id, &assignment.agent_name)
            .await?;

        // Get result file path
        let result_file = self
            .coordinator
            .get_result_path(&plan.task_id, &assignment.agent_name)?;

        // Copy AGENT.md to worktree (if exists)
        let agent_guide_path = self.coordinator.project_root.join(".ergatai/AGENT.md");
        if tokio::fs::try_exists(&agent_guide_path)
            .await
            .unwrap_or(false)
        {
            let worktree_agent_guide = worktree_path.join("AGENT.md");
            tokio::fs::copy(&agent_guide_path, &worktree_agent_guide).await?;
        }

        // Create agent instruction text
        let instruction = self
            .create_agent_instruction(
                &assignment.agent_name,
                &worktree_path,
                &plan.plan_file,
                &result_file,
                assignment,
            )
            .await;

        // Save instruction to file for debugging/auditing
        let instruction_file = worktree_path.join(".ergatai-task.md");
        tokio::fs::write(&instruction_file, &instruction).await?;

        // Create running agent record
        let running_agent = RunningAgent {
            task_id: plan.task_id.clone(),
            agent_name: assignment.agent_name.clone(),
            worktree_path: worktree_path.clone(),
            plan_file: plan.plan_file.clone(),
            result_file: result_file.clone(),
            status: AgentStatus::Starting,
            session_id: None,
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
            &worktree_path,
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
        worktree_path: &Path,
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
{worktree_path}
```

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
2. Work in your designated worktree directory
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

- Do NOT modify files outside your worktree
- Focus only on your assigned objective
- If you encounter issues, document them in your result file
- Complete your task and write the result file when done
"#,
            project_context = project_context,
            agent_name = agent_name,
            worktree_path = worktree_path.display(),
            plan_file = plan_file.display(),
            objective = assignment.objective,
            task_type = task_type_dbg,
            files_section = files_section,
            result_file = result_file.display(),
        )
    }

    /// Format files section for instruction
    fn format_files_section(&self, assignment: &AgentAssignment) -> String {
        let mut sections = Vec::new();

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
    /// Creates an ACP session via `spawn_session_task_with_kind()`, sends the instruction
    /// text as a `SessionCommand::SendPrompt`, then monitors completion via the
    /// reply channel. On completion or failure, updates the `RunningAgent` status
    /// and notifies the DagScheduler if a DAG node_id is associated.
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
        let mut config = crate::agent::config::get_agent_config(agent_name)
            .with_context(|| format!("Failed to load config for agent '{}'", agent_name))?;
        crate::agent::config::normalize_agent_config(&mut config);

        tracing::info!(
            agent = %agent_id,
            command = %config.command,
            worktree = %worktree_path.display(),
            "Spawning ACP session for agent"
        );

        // Create ACP session
        let (session_id_tx, session_id_rx) = oneshot::channel();
        let cwd = worktree_path.to_string_lossy().to_string();
        crate::acp::sdk_session::spawn_session_task_with_kind(config, cwd, SessionKind::Dag, session_id_tx);

        // Wait for session creation
        let session_id = session_id_rx
            .await
            .map_err(|_| anyhow::anyhow!("Session creation channel closed"))?
            .with_context(|| format!("Failed to create ACP session for {}", agent_id))?;

        tracing::info!(
            agent = %agent_id,
            session_id = %session_id,
            "ACP session created"
        );

        // Update RunningAgent with session_id
        {
            let mut agents = self.running_agents.lock().await;
            if let Some(agent) = agents.get_mut(agent_id) {
                agent.status = AgentStatus::Running;
                agent.session_id = Some(session_id.clone());
            }
        }

        // Get command channel for the session
        let cmd_tx = session_manager()
            .get_cmd_tx(&session_id)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("Session {} lost immediately after creation", session_id)
            })?;

        // Build the full instruction with appropriate prompt context
        let full_instruction = if node_id.is_some() {
            // DAG task: inject orchest prompt (teaches agent how to collaborate)
            let dag_prompt = include_str!("../../prompts/dag_orchestration.md");

            // Get list of available agents
            let agents = crate::agent::discovery::discover_acp_runtimes();
            let agent_list = agents
                .iter()
                .map(|a| format!("- **{}** — {}", a.id, a.label))
                .collect::<Vec<_>>()
                .join("\n");

            // Replace {{agent_list}} placeholder
            let dag_prompt = dag_prompt.replace("{{agent_list}}", &agent_list);

            // Combine: DAG orchestration guide + actual task instruction
            format!("{}\n\n---\n\n{}", dag_prompt, instruction)
        } else {
            // Regular session (possibly primary agent): inject generation prompt
            // (teaches agent how to generate DAG when needed)
            let gen_prompt = include_str!("../../prompts/dag_generation.md");

            // Get list of available agents
            let agents = crate::agent::discovery::discover_acp_runtimes();
            let agent_list = agents
                .iter()
                .map(|a| format!("- **{}** — {}", a.id, a.label))
                .collect::<Vec<_>>()
                .join("\n");

            // Replace {{agent_list}} placeholder
            let gen_prompt = gen_prompt.replace("{{agent_list}}", &agent_list);

            // Combine: DAG generation guide + user instruction
            format!("{}\n\n---\n\n{}", gen_prompt, instruction)
        };

        // Send instruction as prompt
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(SessionCommand::SendPrompt {
                text: full_instruction,
                reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "Failed to send prompt to session {} for {}",
                    session_id,
                    agent_id
                )
            })?;

        // Monitor completion in background
        let agent_id_owned = agent_id.to_string();
        let session_id_owned = session_id.clone();
        let running_agents = self.running_agents.clone();
        let node_id_owned = node_id;

        tokio::spawn(async move {
            let completed_ok = match reply_rx.await {
                Ok(Ok(())) => {
                    tracing::info!(
                        agent = %agent_id_owned,
                        session_id = %session_id_owned,
                        "ACP session completed successfully"
                    );
                    let mut agents = running_agents.lock().await;
                    if let Some(agent) = agents.get_mut(&agent_id_owned) {
                        // Monotonic state transition guard
                        if agent.status == AgentStatus::Running
                            || agent.status == AgentStatus::Starting
                        {
                            agent.status = AgentStatus::Completed;
                        }
                    }
                    true
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent = %agent_id_owned,
                        session_id = %session_id_owned,
                        error = %e,
                        "ACP prompt failed"
                    );
                    let mut agents = running_agents.lock().await;
                    if let Some(agent) = agents.get_mut(&agent_id_owned) {
                        if agent.status == AgentStatus::Running
                            || agent.status == AgentStatus::Starting
                        {
                            agent.status = AgentStatus::Failed;
                        }
                    }
                    false
                }
                Err(_) => {
                    tracing::error!(
                        agent = %agent_id_owned,
                        session_id = %session_id_owned,
                        "ACP session reply channel closed (session may have crashed)"
                    );
                    let mut agents = running_agents.lock().await;
                    if let Some(agent) = agents.get_mut(&agent_id_owned) {
                        if agent.status == AgentStatus::Running
                            || agent.status == AgentStatus::Starting
                        {
                            agent.status = AgentStatus::Failed;
                        }
                    }
                    false
                }
            };

            // Notify DagScheduler if this agent is part of a DAG.
            // Prefer NATS event publishing (event-driven, decoupled).
            // Fallback to direct function call if NATS is unavailable.
            if let Some(nid) = node_id_owned {
                if crate::nats::is_nats_initialized().await {
                    // NATS path: publish event, let DagScheduler subscribe and react
                    if let Some(conn) = crate::nats::get_nats_connection().await {
                        let bus = crate::nats::EventBus::new(conn);
                        if completed_ok {
                            let payload = crate::nats::NodeCompletePayload {
                                node_id: nid.clone(),
                                task_id: nid.clone(),
                                agent_name: agent_id_owned.clone(),
                                result_summary: None,
                                outputs: HashMap::new(), // Agent output extraction deferred to Phase 5
                                result_file: None,
                            };
                            if let Err(e) = bus.publish_node_complete(&payload).await {
                                tracing::error!(
                                    node_id = %nid,
                                    error = %e,
                                    "Failed to publish NATS node_complete event"
                                );
                            } else {
                                tracing::info!(
                                    node_id = %nid,
                                    "Published NATS node_complete event"
                                );
                            }
                        } else {
                            let err_msg = format!("ACP session failed for agent {}", agent_id_owned);
                            let payload = crate::nats::NodeFailedPayload {
                                node_id: nid.clone(),
                                task_id: nid.clone(),
                                agent_name: agent_id_owned.clone(),
                                error: err_msg,
                                retryable: false,
                            };
                            if let Err(e) = bus.publish_node_failed(&payload).await {
                                tracing::error!(
                                    node_id = %nid,
                                    error = %e,
                                    "Failed to publish NATS node_failed event"
                                );
                            } else {
                                tracing::info!(
                                    node_id = %nid,
                                    "Published NATS node_failed event"
                                );
                            }
                        }
                    }
                } else if let Some(scheduler) = super::dag_scheduler::get_dag_scheduler() {
                    // Fallback: direct function call (NATS unavailable)
                    // Note: on_node_completed/on_node_failed return !Send futures (they internally
                    // call spawn_acp_session which holds non-Send state across await points).
                    // We use spawn_blocking + block_on to bridge from the current async context.
                    // This is safe because DAG node count is small (<50) and won't exhaust the
                    // blocking thread pool (default 512 threads).
                    let agent_c = agent_id_owned.clone();
                    if completed_ok {
                        let nid_c = nid.clone();
                        tokio::task::spawn_blocking(move || {
                            let rt = tokio::runtime::Handle::current();
                            match rt.block_on(scheduler.on_node_completed(&nid_c, None)) {
                                Ok(newly_submitted) => {
                                    tracing::info!(
                                        node_id = %nid_c,
                                        newly_submitted = ?newly_submitted,
                                        "DAG node completed, triggered downstream (fallback)"
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        node_id = %nid_c,
                                        error = %e,
                                        "DAG scheduler notification failed (completion)"
                                    );
                                }
                            }
                        });
                    } else {
                        let err_msg = format!("ACP session failed for agent {}", agent_c);
                        tokio::task::spawn_blocking(move || {
                            let rt = tokio::runtime::Handle::current();
                            if let Err(e) = rt.block_on(scheduler.on_node_failed(&nid, &err_msg)) {
                                tracing::error!(
                                    node_id = %nid,
                                    error = %e,
                                    "DAG scheduler notification failed (failure)"
                                );
                            }
                        });
                    }
                }
            }
        });

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

    /// Clean up agent resources (ACP session + worktree)
    pub async fn cleanup_agent(&self, agent_id: &str) -> ErgataiResult<()> {
        if let Some(agent) = self.running_agents.lock().await.remove(agent_id) {
            // Close ACP session if still active
            if let Some(ref session_id) = agent.session_id {
                if let Some(cmd_tx) = session_manager().get_cmd_tx(session_id).await {
                    let _ = cmd_tx.send(SessionCommand::Close);
                }
            }

            self.coordinator
                .cleanup_worktree(&agent.task_id, &agent.agent_name)
                .await?;
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
    use crate::cross_agent::task_coordinator::TaskType;

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
            worktree_name: "test-worktree".to_string(),
            depends_on: vec![],
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
            worktree_name: "test".to_string(),
            depends_on: vec![],
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
            worktree_name: "test".to_string(),
            depends_on: vec![],
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

