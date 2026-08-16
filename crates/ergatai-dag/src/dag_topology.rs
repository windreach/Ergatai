//! DAG-based task orchestration
//!
//! A directed acyclic graph where nodes are tasks and edges are dependencies.
//! More flexible than tree: supports parallel branches, convergence, and complex dependencies.
//!
//! Example:
//! ```markdown
//! ## Task A
//! - **agent**: agent-a
//! - **task**: tasks/a.md
//!
//! ## Task B
//! - **agent**: agent-b
//! - **task**: tasks/b.md
//!
//! ## Task C
//! - **agent**: agent-c
//! - **task**: tasks/c.md
//! - **depends_on**: [Task A, Task B]
//! ```

use std::collections::HashMap;

use anyhow::Context;
use ergatai_error::{ErgataiError, ErgataiResult};
use serde::{Deserialize, Serialize};
use tokio::fs;

/// A task in the DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    /// Unique identifier (e.g., "n1", "root", "dev-1")
    pub id: String,

    /// Agent name responsible for this task
    pub agent: String,

    /// Human-readable task description
    pub task: String,

    /// Current status
    pub status: TaskStatus,

    /// Explicit dependencies: this task can only start when all deps are completed
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Optional: input data (can reference other nodes' outputs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,

    /// Optional: output schema or path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,

    /// Optional: result path (set when completed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,

    /// Optional: max retries on failure
    #[serde(default)]
    pub max_retries: u32,

    /// Current retry count
    #[serde(default)]
    pub retry_count: u32,

    /// Optional: execution priority (high / medium / low)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    /// Optional: execution timeout in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Optional: file access scope (glob pattern, e.g., "src/**/*.rs")
    /// Phase 3: For file access control - defines which files this task can access
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,

    /// Optional: metadata (for AI to attach context)
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Task execution status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not started yet (waiting for dependencies)
    Pending,
    /// Currently being executed
    Running,
    /// Successfully completed
    Completed,
    /// Failed (may retry)
    Failed,
    /// Skipped (dependency failed, won't execute)
    Skipped,
}

/// DAG-based task graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
    /// All nodes in the graph (flat structure)
    pub nodes: Vec<TaskNode>,

    /// Optional: when this graph was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,

    /// Optional: description of the overall goal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl TaskGraph {
    /// Create a new task graph
    pub fn new(nodes: Vec<TaskNode>) -> Self {
        Self {
            nodes,
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            description: None,
        }
    }

    /// Find all nodes that are ready to execute
    /// (status=Pending and all dependencies are Completed)
    pub fn ready_tasks(&self) -> Vec<&TaskNode> {
        let completed = self.completed_ids();

        self.nodes
            .iter()
            .filter(|node| {
                matches!(node.status, TaskStatus::Pending)
                    && node
                        .depends_on
                        .iter()
                        .all(|dep| completed.contains(&dep.as_str()))
            })
            .collect()
    }

    /// Get IDs of all completed nodes
    fn completed_ids(&self) -> std::collections::HashSet<&str> {
        self.nodes
            .iter()
            .filter(|n| matches!(n.status, TaskStatus::Completed))
            .map(|n| n.id.as_str())
            .collect()
    }

    /// Find a node by ID
    pub fn find_node(&self, id: &str) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Find a mutable node by ID
    pub fn find_node_mut(&mut self, id: &str) -> Option<&mut TaskNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Update a node's status by ID
    pub fn update_status(&mut self, id: &str, status: TaskStatus) -> ErgataiResult<()> {
        let node = self
            .find_node_mut(id)
            .with_context(|| format!("Node not found: {}", id))?;
        node.status = status;
        Ok(())
    }

    /// Set result path for a completed node
    pub fn set_result(&mut self, id: &str, result_path: String) -> ErgataiResult<()> {
        let node = self
            .find_node_mut(id)
            .with_context(|| format!("Node not found: {}", id))?;
        node.result_path = Some(result_path);
        node.status = TaskStatus::Completed;
        Ok(())
    }

    /// Increment retry count for a failed node
    pub fn retry_failed(&mut self, id: &str) -> ErgataiResult<bool> {
        let node = self
            .find_node_mut(id)
            .with_context(|| format!("Node not found: {}", id))?;

        if node.retry_count < node.max_retries {
            node.retry_count += 1;
            node.status = TaskStatus::Pending;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get overall progress (0.0 to 1.0)
    pub fn progress(&self) -> f32 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        let completed = self
            .nodes
            .iter()
            .filter(|n| matches!(n.status, TaskStatus::Completed))
            .count();
        completed as f32 / self.nodes.len() as f32
    }

    /// Check if all tasks are completed (or failed/skipped)
    pub fn is_complete(&self) -> bool {
        self.nodes.iter().all(|n| {
            matches!(
                n.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Skipped
            )
        })
    }

    /// Validate the DAG (check for cycles and missing dependencies)
    pub fn validate(&self) -> ErgataiResult<()> {
        // Check for duplicate IDs
        let mut seen_ids = std::collections::HashSet::new();
        for node in &self.nodes {
            if !seen_ids.insert(node.id.as_str()) {
                return Err(ErgataiError::InvalidArgument(format!(
                    "Duplicate node ID: {}",
                    node.id
                )));
            }
        }

        // Check for missing dependencies (O(N) with HashSet lookup)
        let all_ids: std::collections::HashSet<&str> =
            self.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &self.nodes {
            for dep in &node.depends_on {
                if !all_ids.contains(dep.as_str()) {
                    return Err(ErgataiError::InvalidArgument(format!(
                        "Node {} depends on {}, which doesn't exist",
                        node.id, dep
                    )));
                }
            }
        }

        // Check for cycles using topological sort
        if self.has_cycle() {
            return Err(ErgataiError::InvalidArgument(
                "Graph has cycles".to_string(),
            ));
        }

        Ok(())
    }

    /// Detect cycles using DFS
    fn has_cycle(&self) -> bool {
        let mut visited = std::collections::HashSet::with_capacity(self.nodes.len());
        let mut rec_stack = std::collections::HashSet::with_capacity(self.nodes.len());

        for node in &self.nodes {
            if self.dfs_cycle(&node.id, &mut visited, &mut rec_stack) {
                return true;
            }
        }
        false
    }

    /// DFS helper for cycle detection.
    ///
    /// Uses `HashSet<String>` because `dep` borrows from `node.depends_on`
    /// (tied to `&self` lifetime), which differs from the `id` lifetime.
    /// For typical DAG sizes (< 100 nodes), the allocation overhead is negligible.
    fn dfs_cycle(
        &self,
        id: &str,
        visited: &mut std::collections::HashSet<String>,
        rec_stack: &mut std::collections::HashSet<String>,
    ) -> bool {
        visited.insert(id.to_string());
        rec_stack.insert(id.to_string());

        if let Some(node) = self.find_node(id) {
            for dep in &node.depends_on {
                if !visited.contains(dep.as_str()) {
                    if self.dfs_cycle(dep, visited, rec_stack) {
                        return true;
                    }
                } else if rec_stack.contains(dep.as_str()) {
                    return true;
                }
            }
        }

        rec_stack.remove(id);
        false
    }

    /// Serialize to AI-friendly format
    pub fn to_ai_prompt(&self) -> String {
        use std::fmt::Write;
        let mut output = String::with_capacity(256);

        if let Some(desc) = &self.description {
            let _ = writeln!(output, "Goal: {}\n", desc);
        }

        output.push_str("Task Graph:\n");
        for node in &self.nodes {
            let status_icon = match node.status {
                TaskStatus::Pending => "⏳",
                TaskStatus::Running => "🔄",
                TaskStatus::Completed => "✅",
                TaskStatus::Failed => "❌",
                TaskStatus::Skipped => "⏭️",
            };

            // Show task path if available, otherwise show task description
            let task_ref = node
                .metadata
                .get("task_path")
                .map(|p| p.as_str())
                .unwrap_or(&node.task);

            let _ = write!(output, "{} {} → {}", status_icon, node.agent, task_ref);

            if !node.depends_on.is_empty() {
                let _ = write!(output, " (depends: {})", node.depends_on.join(", "));
            }

            output.push('\n');
        }

        let _ = writeln!(output, "\nProgress: {:.0}%", self.progress() * 100.0);

        let ready = self.ready_tasks();
        if !ready.is_empty() {
            output.push_str("Ready to execute:\n");
            for node in ready {
                let task_ref = node
                    .metadata
                    .get("task_path")
                    .map(|p| p.as_str())
                    .unwrap_or(&node.task);
                let _ = writeln!(output, "  - {} ({})", node.agent, task_ref);
            }
        }

        output
    }

    /// Save to file
    pub async fn save_to_file(&self, path: &std::path::Path) -> ErgataiResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).await?;
        Ok(())
    }

    /// Load from file
    pub async fn load_from_file(path: &std::path::Path) -> ErgataiResult<Self> {
        let content = fs::read_to_string(path).await?;
        let graph: Self = serde_json::from_str(&content)?;
        Ok(graph)
    }
}

impl TaskNode {
    /// Create a new task node
    pub fn new(id: impl Into<String>, agent: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            agent: agent.into(),
            task: task.into(),
            status: TaskStatus::Pending,
            depends_on: Vec::new(),
            input: None,
            output: None,
            result_path: None,
            max_retries: 0,
            retry_count: 0,
            priority: None,
            timeout: None,
            scope: None, // Phase 3: File access scope (default: None)
            metadata: HashMap::new(),
        }
    }

    /// Add dependencies
    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.depends_on = deps;
        self
    }

    /// Set input
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = Some(input.into());
        self
    }

    /// Set max retries
    pub fn with_max_retries(mut self, max: u32) -> Self {
        self.max_retries = max;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> TaskGraph {
        TaskGraph::new(vec![
            TaskNode::new("n1", "agent-a", "Task A"),
            TaskNode::new("n2", "agent-b", "Task B"),
            TaskNode::new("n3", "agent-c", "Task C")
                .with_dependencies(vec!["n1".into(), "n2".into()]),
        ])
    }

    #[test]
    fn test_ready_tasks_initial() {
        let graph = sample_graph();
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 2); // n1 and n2 (n3 depends on them)
    }

    #[test]
    fn test_ready_tasks_after_completion() {
        let mut graph = sample_graph();
        graph.update_status("n1", TaskStatus::Completed).unwrap();

        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1); // only n2 (n3 still waiting for n2)
    }

    #[test]
    fn test_ready_tasks_all_deps_done() {
        let mut graph = sample_graph();
        graph.update_status("n1", TaskStatus::Completed).unwrap();
        graph.update_status("n2", TaskStatus::Completed).unwrap();

        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1); // now n3 is ready
        assert_eq!(ready[0].id, "n3");
    }

    #[test]
    fn test_progress() {
        let mut graph = sample_graph();
        assert_eq!(graph.progress(), 0.0);

        graph.update_status("n1", TaskStatus::Completed).unwrap();
        assert!((graph.progress() - 0.333).abs() < 0.01);

        graph.update_status("n2", TaskStatus::Completed).unwrap();
        assert!((graph.progress() - 0.666).abs() < 0.01);

        graph.update_status("n3", TaskStatus::Completed).unwrap();
        assert_eq!(graph.progress(), 1.0);
    }

    #[test]
    fn test_validation() {
        let graph = sample_graph();
        assert!(graph.validate().is_ok());
    }

    #[test]
    fn test_missing_dependency() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent", "Task").with_dependencies(vec!["missing".into()])
        ]);
        assert!(graph.validate().is_err());
    }

    #[test]
    fn test_retry() {
        let mut graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent", "Task").with_max_retries(3)
        ]);

        graph.update_status("n1", TaskStatus::Failed).unwrap();
        assert!(graph.retry_failed("n1").unwrap());
        assert_eq!(graph.find_node("n1").unwrap().status, TaskStatus::Pending);
        assert_eq!(graph.find_node("n1").unwrap().retry_count, 1);
    }

    #[test]
    fn test_skipped_status_exists() {
        let status = TaskStatus::Skipped;
        assert_eq!(status, TaskStatus::Skipped);
    }

    #[test]
    fn test_is_complete_with_skipped() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent", "Task 1"),
            TaskNode::new("n2", "agent", "Task 2"),
        ]);

        let mut graph = graph;
        graph.update_status("n1", TaskStatus::Completed).unwrap();
        graph.update_status("n2", TaskStatus::Skipped).unwrap();

        // Should be complete (Completed + Skipped = all done)
        assert!(graph.is_complete());
    }
}
