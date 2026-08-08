// Task Coordinator - File-based cross-agent collaboration
// Manages task plans, git worktrees, and agent coordination

use std::path::{Path, PathBuf};

use anyhow::Context;
use crate::error::{ErgataiError, ErgataiResult};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::process::Command;

/// Validate that a string is safe to use as a path component.
///
/// Rejects `..`, slashes, backslashes, and any character that could escape the
/// `.ergatai/` directory or cause surprising filesystem behavior. Called at the
/// public-API boundary of every function that interpolates `task_id` or
/// `agent_name` into a path.
fn validate_path_component(name: &str, label: &str) -> ErgataiResult<()> {
    if name.is_empty() {
        return Err(ErgataiError::InvalidArgument(format!("{} must not be empty", label)));
    }
    if name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('|')
        || name.contains('*')
        || name.contains('?')
    {
        return Err(ErgataiError::InvalidArgument(format!(
            "{} contains invalid characters (refusing path traversal): {:?}",
            label,
            name
        )));
    }
    Ok(())
}

/// Task assignment for a specific agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAssignment {
    pub agent_name: String,
    pub objective: String,
    pub files_to_create: Vec<PathBuf>,
    pub files_to_modify: Vec<PathBuf>,
    pub files_to_read: Vec<PathBuf>,
    pub task_type: TaskType,
    pub worktree_name: String,

    // DAG support: dependencies (ID is auto-generated UUID)
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Type of task (determines worktree isolation level)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TaskType {
    /// Agent only reads files, outputs result to result file
    ReadOnly,
    /// Agent creates new files (low conflict risk)
    CreateNew,
    /// Agent modifies existing files (high conflict risk)
    ModifyExisting,
}

/// Status of a task plan
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanStatus {
    InProgress,
    Completed,
    Failed,
}

/// Parsed task plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub task_id: String,
    pub task_name: String,
    pub coordinator: String,
    pub status: PlanStatus,
    pub assignments: Vec<AgentAssignment>,
    pub merge_strategy: String,
    pub plan_file: PathBuf,
}

/// Result of merging a worktree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    pub success: bool,
    pub conflicts: Vec<PathBuf>,
    pub merged_files: Vec<PathBuf>,
    pub error: Option<String>,
}

/// Worktree status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeStatus {
    pub modified_files: Vec<PathBuf>,
    pub new_files: Vec<PathBuf>,
    pub deleted_files: Vec<PathBuf>,
    pub has_changes: bool,
}

/// Task Coordinator - manages cross-agent collaboration
pub struct TaskCoordinator {
    pub project_root: PathBuf,
    plan_dir: PathBuf,
    worktree_dir: PathBuf,
    results_dir: PathBuf,
}

impl TaskCoordinator {
    /// Create a new TaskCoordinator
    pub fn new(project_root: PathBuf) -> Self {
        let ergatai_dir = project_root.join(".ergatai");
        let plan_dir = ergatai_dir.join(".plan");
        let worktree_dir = ergatai_dir.join("worktrees");
        let results_dir = plan_dir.join("results");

        Self {
            project_root,
            plan_dir,
            worktree_dir,
            results_dir,
        }
    }

    /// Initialize directories
    pub async fn init(&self) -> ErgataiResult<()> {
        fs::create_dir_all(&self.plan_dir).await?;
        fs::create_dir_all(&self.worktree_dir).await?;
        fs::create_dir_all(&self.results_dir).await?;
        Ok(())
    }

    /// Create a new task plan file
    pub async fn create_plan(&self, task_id: &str, content: &str) -> ErgataiResult<PathBuf> {
        validate_path_component(task_id, "task_id")?;
        let plan_file = self.plan_dir.join(format!("{}.md", task_id));
        fs::write(&plan_file, content).await?;
        Ok(plan_file)
    }

    /// Parse a task plan file
    pub async fn parse_plan(&self, plan_file: &Path) -> ErgataiResult<TaskPlan> {
        let content = fs::read_to_string(plan_file)
            .await
            .with_context(|| format!("Failed to read plan file: {:?}", plan_file))?;

        let task_id = plan_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse markdown to extract task info and assignments
        let task_name = extract_task_name(&content).unwrap_or_else(|| "Unknown Task".to_string());
        let coordinator = extract_coordinator(&content).unwrap_or_else(|| "unknown".to_string());
        let assignments = parse_assignments(&content, &task_id)?;
        let merge_strategy = extract_merge_strategy(&content)
            .unwrap_or_else(|| "Main agent handles conflicts".to_string());

        Ok(TaskPlan {
            task_id,
            task_name,
            coordinator,
            status: PlanStatus::InProgress,
            assignments,
            merge_strategy,
            plan_file: plan_file.to_path_buf(),
        })
    }

    /// Create git worktree for an agent
    pub async fn create_worktree(&self, task_id: &str, agent: &str) -> ErgataiResult<PathBuf> {
        validate_path_component(task_id, "task_id")?;
        validate_path_component(agent, "agent")?;
        let worktree_name = format!("{}-{}", task_id, agent);
        let worktree_path = self.worktree_dir.join(&worktree_name);

        // Remove existing worktree if it exists
        if tokio::fs::try_exists(&worktree_path).await.unwrap_or(false) {
            let status = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&worktree_path)
                .current_dir(&self.project_root)
                .status()
                .await
                .with_context(|| "Failed to invoke git worktree remove")?;
            if !status.success() {
                tracing::warn!(
                    ?worktree_path,
                    status = ?status,
                    "git worktree remove returned non-zero (continuing)"
                );
            }
        }

        // Create new worktree based on the main repo's current HEAD.
        // `--detach` avoids assuming a branch name (the project may use `main`,
        // `master`, `trunk`, or anything else) and works regardless of which
        // branch is currently checked out in the main worktree. The agent gets
        // a detached HEAD at that commit; `merge_worktree` later creates a
        // real branch via `git checkout -b` before merging back.
        let output = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&worktree_path)
            .current_dir(&self.project_root)
            .output()
            .await
            .with_context(|| "Failed to create git worktree")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ErgataiError::internal(format!("Git worktree add failed: {}", stderr)));
        }

        Ok(worktree_path)
    }

    /// Get worktree status (changed files)
    pub async fn get_worktree_status(&self, worktree_path: &Path) -> ErgataiResult<WorktreeStatus> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree_path)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut modified_files = Vec::new();
        let mut new_files = Vec::new();
        let mut deleted_files = Vec::new();

        for line in stdout.lines() {
            if line.len() < 3 {
                continue;
            }
            let status = &line[0..2];
            let file = line[3..].to_string();
            let path = PathBuf::from(file);

            match status.trim() {
                "M" | "MM" => modified_files.push(path),
                "A" | "AM" | "??" => new_files.push(path),
                "D" => deleted_files.push(path),
                _ => {}
            }
        }

        let has_changes = !modified_files.is_empty() || !new_files.is_empty() || !deleted_files.is_empty();

        Ok(WorktreeStatus {
            modified_files,
            new_files,
            deleted_files,
            has_changes,
        })
    }

    /// Merge worktree changes to main branch
    pub async fn merge_worktree(&self, task_id: &str, agent: &str) -> ErgataiResult<MergeResult> {
        validate_path_component(task_id, "task_id")?;
        validate_path_component(agent, "agent")?;
        let worktree_name = format!("{}-{}", task_id, agent);
        let worktree_path = self.worktree_dir.join(&worktree_name);

        if !tokio::fs::try_exists(&worktree_path).await.unwrap_or(false) {
            return Ok(MergeResult {
                success: false,
                conflicts: vec![],
                merged_files: vec![],
                error: Some(format!("Worktree not found: {}", worktree_name)),
            });
        }

        // Check for changes
        let status = self.get_worktree_status(&worktree_path).await?;
        if !status.has_changes {
            return Ok(MergeResult {
                success: true,
                conflicts: vec![],
                merged_files: vec![],
                error: None,
            });
        }

        // Commit changes in worktree — each step must succeed before moving on.
        let branch_name = format!("task-{}-{}", task_id, agent);

        let checkout_status = Command::new("git")
            .args(["checkout", "-b", &branch_name])
            .current_dir(&worktree_path)
            .output()
            .await
            .with_context(|| "Failed to invoke git checkout -b")?;
        if !checkout_status.status.success() {
            let stderr = String::from_utf8_lossy(&checkout_status.stderr);
            return Err(ErgataiError::internal(format!("git checkout -b {} failed: {}", branch_name, stderr)));
        }

        let add_status = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&worktree_path)
            .output()
            .await
            .with_context(|| "Failed to invoke git add")?;
        if !add_status.status.success() {
            let stderr = String::from_utf8_lossy(&add_status.stderr);
            return Err(ErgataiError::internal(format!("git add -A failed: {}", stderr)));
        }

        let commit_msg = format!("Task {}: {} completion", task_id, agent);
        let commit_status = Command::new("git")
            .args(["commit", "-m", &commit_msg, "--allow-empty"])
            .current_dir(&worktree_path)
            .output()
            .await
            .with_context(|| "Failed to invoke git commit")?;
        if !commit_status.status.success() {
            let stderr = String::from_utf8_lossy(&commit_status.stderr);
            return Err(ErgataiError::internal(format!("git commit failed: {}", stderr)));
        }

        // Try to merge to main
        let merge_output = Command::new("git")
            .args(["merge", &branch_name, "--no-edit"])
            .current_dir(&self.project_root)
            .output()
            .await?;

        if !merge_output.status.success() {
            // Merge conflict
            let conflict_output = Command::new("git")
                .args(["diff", "--name-only", "--diff-filter=U"])
                .current_dir(&self.project_root)
                .output()
                .await?;

            let conflicts = String::from_utf8_lossy(&conflict_output.stdout)
                .lines()
                .map(PathBuf::from)
                .collect();

            // Abort merge
            Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.project_root)
                .status()
                .await?;

            return Ok(MergeResult {
                success: false,
                conflicts,
                merged_files: vec![],
                error: Some("Merge conflict detected".to_string()),
            });
        }

        // Successful merge
        let merged_files = status
            .modified_files
            .into_iter()
            .chain(status.new_files)
            .collect();

        Ok(MergeResult {
            success: true,
            conflicts: vec![],
            merged_files,
            error: None,
        })
    }

    /// Clean up worktree
    pub async fn cleanup_worktree(&self, task_id: &str, agent: &str) -> ErgataiResult<()> {
        validate_path_component(task_id, "task_id")?;
        validate_path_component(agent, "agent")?;
        let worktree_name = format!("{}-{}", task_id, agent);
        let worktree_path = self.worktree_dir.join(&worktree_name);

        if tokio::fs::try_exists(&worktree_path).await.unwrap_or(false) {
            let status = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&worktree_path)
                .current_dir(&self.project_root)
                .status()
                .await
                .with_context(|| "Failed to invoke git worktree remove")?;
            if !status.success() {
                tracing::warn!(
                    ?worktree_path,
                    status = ?status,
                    "git worktree remove returned non-zero during cleanup"
                );
            }
        }

        Ok(())
    }

    /// Clean up all worktrees for a task
    pub async fn cleanup_task(&self, task_id: &str) -> ErgataiResult<()> {
        validate_path_component(task_id, "task_id")?;
        let pattern = format!("{}-", task_id);
        if let Ok(mut entries) = fs::read_dir(&self.worktree_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with(&pattern) {
                            let status = Command::new("git")
                                .args(["worktree", "remove", "--force"])
                                .arg(&path)
                                .current_dir(&self.project_root)
                                .status()
                                .await;
                            if let Err(e) = status {
                                tracing::warn!(
                                    ?path,
                                    error = %e,
                                    "Failed to invoke git worktree remove during task cleanup"
                                );
                            }
                        }
                    }
                }
            }
        }

        // Clean up plan file and results
        let plan_file = self.plan_dir.join(format!("{}.md", task_id));
        if tokio::fs::try_exists(&plan_file).await.unwrap_or(false) {
            fs::remove_file(&plan_file).await?;
        }

        if let Ok(mut entries) = fs::read_dir(&self.results_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with(&pattern) {
                        fs::remove_file(&path).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if all assignments in a plan are completed
    pub async fn check_completion(&self, plan: &TaskPlan) -> ErgataiResult<bool> {
        validate_path_component(&plan.task_id, "task_id")?;
        for assignment in &plan.assignments {
            validate_path_component(&assignment.agent_name, "agent")?;
            let result_file = self.results_dir.join(format!(
                "{}-{}.md",
                plan.task_id, assignment.agent_name
            ));
            if !tokio::fs::try_exists(&result_file).await.unwrap_or(false) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Get the result file path for an agent
    pub fn get_result_path(&self, task_id: &str, agent: &str) -> ErgataiResult<PathBuf> {
        validate_path_component(task_id, "task_id")?;
        validate_path_component(agent, "agent")?;
        Ok(self.results_dir.join(format!("{}-{}.md", task_id, agent)))
    }
}

// Helper functions for parsing markdown

fn extract_task_name(content: &str) -> Option<String> {
    // Look for "# Task: [name]" pattern
    for line in content.lines() {
        if line.starts_with("# Task:") || line.starts_with("# 任务：") {
            let name = line
                .split_once(':')
                .or_else(|| line.split_once('：'))
                .map(|(_, name)| name.trim().to_string());
            return name;
        }
    }
    None
}

fn extract_coordinator(content: &str) -> Option<String> {
    // Look for "**Coordinator**: [name]" pattern
    for line in content.lines() {
        if line.contains("**Coordinator**:") || line.contains("**主 Agent**:") {
            if let Some(name) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                return Some(name.trim().to_string());
            }
        }
    }
    None
}

fn extract_merge_strategy(content: &str) -> Option<String> {
    // Look for "## Merge Strategy" section
    let mut in_section = false;
    for line in content.lines() {
        if line.starts_with("## Merge Strategy") || line.starts_with("## 合并策略") {
            in_section = true;
            continue;
        }
        if in_section {
            if line.starts_with("## ") {
                break;
            }
            if !line.trim().is_empty() {
                return Some(line.trim().to_string());
            }
        }
    }
    None
}

fn parse_assignments(content: &str, task_id: &str) -> ErgataiResult<Vec<AgentAssignment>> {
    let mut assignments = Vec::new();
    let mut current_assignment: Option<AgentAssignmentBuilder> = None;

    for line in content.lines() {
        // Check for assignment header: ### @agent - [title]
        if line.starts_with("### @") {
            // Save previous assignment
            if let Some(builder) = current_assignment.take() {
                assignments.push(builder.build()?);
            }

            // Parse agent name
            let agent_part = line.trim_start_matches("### @");
            if let Some(agent_name) = agent_part.split_whitespace().next() {
                let agent_name = agent_name.trim_end_matches('-').trim().to_string();
                let worktree_name = format!("{}-{}", task_id, agent_name);
                let builder = AgentAssignmentBuilder::new(agent_name, worktree_name);

                current_assignment = Some(builder);
            }
        } else if let Some(ref mut builder) = current_assignment {
            // Parse assignment details
            if line.contains("**Objective**:") || line.contains("**目标**:") {
                if let Some(obj) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.objective = Some(obj.trim().to_string());
                }
            } else if line.contains("**Type**:") || line.contains("**类型**:") {
                if let Some(task_type) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.task_type = Some(parse_task_type(task_type.trim()));
                }
            } else if line.contains("**Files to create**:") || line.contains("**创建**:") {
                if let Some(files) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.files_to_create.extend(parse_file_list(files));
                }
            } else if line.contains("**Files to modify**:") || line.contains("**修改**:") {
                if let Some(files) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.files_to_modify.extend(parse_file_list(files));
                }
            } else if line.contains("**Files to read**:") || line.contains("**只读**:") {
                if let Some(files) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.files_to_read.extend(parse_file_list(files));
                }
            } else if line.contains("**depends_on**:") || line.contains("**依赖**:") {
                // Parse depends_on: [id1, id2, id3]
                if let Some(deps) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.depends_on = parse_depends_on(deps.trim());
                }
            }
        }
    }

    // Save last assignment
    if let Some(builder) = current_assignment {
        assignments.push(builder.build()?);
    }

    Ok(assignments)
}

/// Parse depends_on array: "[id1, id2, id3]" -> Vec<String>
fn parse_depends_on(s: &str) -> Vec<String> {
    let trimmed = s.trim();

    // Remove [ and ]
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    // Split by comma and clean up
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl TaskPlan {
    /// Convert TaskPlan to TaskGraph for DAG-based scheduling
    /// Node IDs are auto-generated UUIDs
    pub fn to_task_graph(&self) -> crate::orchestration::TaskGraph {
        use crate::orchestration::{TaskNode, TaskGraph};
        use std::collections::HashMap;
        use uuid::Uuid;

        // First pass: create nodes and build agent_name -> UUID mapping
        let mut name_to_uuid: HashMap<String, String> = HashMap::new();
        let mut nodes: Vec<TaskNode> = Vec::new();

        for assignment in &self.assignments {
            let id = Uuid::new_v4().to_string();
            name_to_uuid.insert(assignment.agent_name.clone(), id.clone());

            let mut node = TaskNode::new(id, &assignment.agent_name, &assignment.objective);
            // Store depends_on temporarily with agent names (will be converted below)
            node.depends_on = assignment.depends_on.clone();

            // Store file info in metadata
            if !assignment.files_to_create.is_empty() {
                node.metadata.insert(
                    "files_to_create".to_string(),
                    assignment
                        .files_to_create
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            if !assignment.files_to_modify.is_empty() {
                node.metadata.insert(
                    "files_to_modify".to_string(),
                    assignment
                        .files_to_modify
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            if !assignment.files_to_read.is_empty() {
                node.metadata.insert(
                    "files_to_read".to_string(),
                    assignment
                        .files_to_read
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }

            nodes.push(node);
        }

        // Second pass: update depends_on references to use UUIDs
        for node in &mut nodes {
            node.depends_on = node
                .depends_on
                .iter()
                .map(|name| name_to_uuid.get(name).cloned().unwrap_or_else(|| name.clone()))
                .collect();
        }

        let mut graph = TaskGraph::new(nodes);
        graph.description = Some(self.task_name.clone());
        graph
    }
}

fn parse_task_type(s: &str) -> TaskType {
    let s_lower = s.to_lowercase();
    if s_lower.contains("read") || s_lower.contains("只读") {
        TaskType::ReadOnly
    } else if s_lower.contains("create") || s_lower.contains("创建") {
        TaskType::CreateNew
    } else if s_lower.contains("modify") || s_lower.contains("修改") {
        TaskType::ModifyExisting
    } else {
        TaskType::ReadOnly
    }
}

fn parse_file_list(s: &str) -> Vec<PathBuf> {
    s.split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .map(PathBuf::from)
        .collect()
}

struct AgentAssignmentBuilder {
    agent_name: String,
    worktree_name: String,
    objective: Option<String>,
    files_to_create: Vec<PathBuf>,
    files_to_modify: Vec<PathBuf>,
    files_to_read: Vec<PathBuf>,
    task_type: Option<TaskType>,
    depends_on: Vec<String>,
}

impl AgentAssignmentBuilder {
    fn new(agent_name: String, worktree_name: String) -> Self {
        Self {
            agent_name,
            worktree_name,
            objective: None,
            files_to_create: Vec::new(),
            files_to_modify: Vec::new(),
            files_to_read: Vec::new(),
            task_type: None,
            depends_on: Vec::new(),
        }
    }

    fn build(self) -> ErgataiResult<AgentAssignment> {
        let task_type = self.task_type.unwrap_or(TaskType::ReadOnly);
        let objective = self.objective.unwrap_or_else(|| "No objective specified".to_string());

        Ok(AgentAssignment {
            agent_name: self.agent_name,
            objective,
            files_to_create: self.files_to_create,
            files_to_modify: self.files_to_modify,
            files_to_read: self.files_to_read,
            task_type,
            worktree_name: self.worktree_name,
            depends_on: self.depends_on,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_task_name() {
        let content = "# Task: Implement User Authentication\n\nSome content";
        assert_eq!(
            extract_task_name(content),
            Some("Implement User Authentication".to_string())
        );
    }

    #[test]
    fn test_parse_assignments() {
        let content = r#"
# Task: Test

### @codex - Security Review
- **Objective**: Review auth code
- **Type**: Read-only

### @test - Unit Tests
- **Objective**: Write tests
- **Type**: CreateNew
- **Files to create**: src/test.ts
"#;

        let assignments = parse_assignments(content, "task-001").unwrap();
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].agent_name, "codex");
        assert_eq!(assignments[0].task_type, TaskType::ReadOnly);
        assert_eq!(assignments[1].agent_name, "test");
        assert_eq!(assignments[1].task_type, TaskType::CreateNew);
    }

    #[test]
    fn test_validate_path_component_accepts_safe_inputs() {
        assert!(validate_path_component("task-001", "task_id").is_ok());
        assert!(validate_path_component("claude-code", "agent").is_ok());
        assert!(validate_path_component("a_b_c", "agent").is_ok());
        assert!(validate_path_component("Task.2024.01", "task_id").is_ok());
    }

    #[test]
    fn test_validate_path_component_rejects_traversal() {
        assert!(validate_path_component("../etc", "task_id").is_err());
        assert!(validate_path_component("foo/../bar", "task_id").is_err());
        assert!(validate_path_component("..", "task_id").is_err());
    }

    #[test]
    fn test_validate_path_component_rejects_slashes_and_special_chars() {
        assert!(validate_path_component("a/b", "task_id").is_err());
        assert!(validate_path_component("a\\b", "task_id").is_err());
        assert!(validate_path_component("a:b", "task_id").is_err());
        assert!(validate_path_component("a|b", "task_id").is_err());
        assert!(validate_path_component("a*b", "task_id").is_err());
        assert!(validate_path_component("a?b", "task_id").is_err());
        assert!(validate_path_component("", "task_id").is_err());
    }

    #[tokio::test]
    async fn test_create_plan_rejects_malicious_task_id() {
        let coordinator = TaskCoordinator::new(std::env::temp_dir());
        // Should fail before touching the filesystem
        let result = coordinator.create_plan("../../evil", "content").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("invalid characters"),
            "unexpected error: {}",
            msg
        );
    }
}
