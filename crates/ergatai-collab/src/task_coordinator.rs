// Task Coordinator - File-based cross-agent collaboration
// Manages task plans and agent coordination with file access control

use std::path::{Path, PathBuf};

use ergatai_error::{ErgataiError, ErgataiResult};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::fs;

/// Join paths into a comma-separated string without intermediate Vec allocation.
fn join_paths(paths: &[PathBuf]) -> String {
    let mut s = String::new();
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&p.to_string_lossy());
    }
    s
}

/// Validate that a string is safe to use as a path component.
///
/// Rejects `..`, slashes, backslashes, and any character that could escape the
/// `.ergatai/` directory or cause surprising filesystem behavior. Called at the
/// public-API boundary of every function that interpolates `task_id` or
/// `agent_name` into a path.
fn validate_path_component(name: &str, label: &str) -> ErgataiResult<()> {
    if name.is_empty() {
        return Err(ErgataiError::InvalidArgument(format!(
            "{} must not be empty",
            label
        )));
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
            label, name
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

    // DAG support: dependencies (ID is auto-generated UUID)
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Task priority from DAG node ("high", "medium", "low")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// Type of task (determines file access level)
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

/// Task Coordinator - manages cross-agent collaboration
pub struct TaskCoordinator {
    pub project_root: PathBuf,
    plan_dir: PathBuf,
    results_dir: PathBuf,
}

impl TaskCoordinator {
    /// Create a new TaskCoordinator
    pub fn new(project_root: PathBuf) -> Self {
        let ergatai_dir = project_root.join(".ergatai");
        let plan_dir = ergatai_dir.join(".plan");
        let results_dir = plan_dir.join("results");

        Self {
            project_root,
            plan_dir,
            results_dir,
        }
    }

    /// Initialize directories
    pub async fn init(&self) -> ErgataiResult<()> {
        fs::create_dir_all(&self.plan_dir).await?;
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
        let assignments = parse_assignments(&content)?;
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

    /// Clean up task files (plan and results)
    pub async fn cleanup_task(&self, task_id: &str) -> ErgataiResult<()> {
        validate_path_component(task_id, "task_id")?;
        let pattern = format!("{}-", task_id);

        // Clean up plan file
        let plan_file = self.plan_dir.join(format!("{}.md", task_id));
        if tokio::fs::try_exists(&plan_file).await.unwrap_or(false) {
            fs::remove_file(&plan_file).await?;
        }

        // Clean up result files
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
            let result_file = self
                .results_dir
                .join(format!("{}-{}.md", plan.task_id, assignment.agent_name));
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

fn parse_assignments(content: &str) -> ErgataiResult<Vec<AgentAssignment>> {
    // Pre-allocate with a reasonable default
    let mut assignments = Vec::with_capacity(4);
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
                let builder = AgentAssignmentBuilder::new(agent_name);

                current_assignment = Some(builder);
            }
        } else if let Some(ref mut builder) = current_assignment {
            // Parse assignment details
            if line.contains("**Objective**:") || line.contains("**目标**:") {
                if let Some(obj) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    builder.objective = Some(obj.trim().to_string());
                }
            } else if line.contains("**Type**:") || line.contains("**类型**:") {
                if let Some(task_type) = line.split(':').nth(1).or_else(|| line.split('：').nth(1))
                {
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
            } else if line.contains("**priority**:") || line.contains("**优先级**:") {
                if let Some(pri) = line.split(':').nth(1).or_else(|| line.split('：').nth(1)) {
                    let pri = pri.trim().to_lowercase();
                    if ["high", "medium", "low"].contains(&pri.as_str()) {
                        builder.priority = Some(pri);
                    }
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
    pub fn to_task_graph(&self) -> ergatai_dag::TaskGraph {
        use ergatai_dag::{TaskGraph, TaskNode};
        use std::collections::HashMap;
        use uuid::Uuid;

        // First pass: create nodes and build agent_name -> UUID mapping
        let mut name_to_uuid: HashMap<String, String> = HashMap::new();
        let mut nodes: Vec<TaskNode> = Vec::with_capacity(self.assignments.len());

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
                    join_paths(&assignment.files_to_create),
                );
            }
            if !assignment.files_to_modify.is_empty() {
                node.metadata.insert(
                    "files_to_modify".to_string(),
                    join_paths(&assignment.files_to_modify),
                );
            }
            if !assignment.files_to_read.is_empty() {
                node.metadata.insert(
                    "files_to_read".to_string(),
                    join_paths(&assignment.files_to_read),
                );
            }

            nodes.push(node);
        }

        // Second pass: update depends_on references to use UUIDs
        for node in &mut nodes {
            node.depends_on = node
                .depends_on
                .iter()
                .map(|name| {
                    name_to_uuid
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.clone())
                })
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
    objective: Option<String>,
    files_to_create: Vec<PathBuf>,
    files_to_modify: Vec<PathBuf>,
    files_to_read: Vec<PathBuf>,
    task_type: Option<TaskType>,
    depends_on: Vec<String>,
    priority: Option<String>,
}

impl AgentAssignmentBuilder {
    fn new(agent_name: String) -> Self {
        Self {
            agent_name,
            objective: None,
            files_to_create: Vec::new(),
            files_to_modify: Vec::new(),
            files_to_read: Vec::new(),
            task_type: None,
            depends_on: Vec::new(),
            priority: None,
        }
    }

    fn build(self) -> ErgataiResult<AgentAssignment> {
        let task_type = self.task_type.unwrap_or(TaskType::ReadOnly);
        let objective = self
            .objective
            .unwrap_or_else(|| "No objective specified".to_string());

        Ok(AgentAssignment {
            agent_name: self.agent_name,
            objective,
            files_to_create: self.files_to_create,
            files_to_modify: self.files_to_modify,
            files_to_read: self.files_to_read,
            task_type,
            depends_on: self.depends_on,
            priority: self.priority,
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

        let assignments = parse_assignments(content).unwrap();
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
