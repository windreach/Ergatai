//! Tree-based task topology for multi-agent orchestration
//!
//! A hierarchical structure where:
//! - Parent must complete before children can start
//! - Siblings at the same level can run in parallel
//! - Optional sibling links allow peer communication
//!
//! This is intentionally simple: AI can reason about trees easily,
//! and most collaboration patterns fit this model.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use anyhow::Context;
use ergatai_error::ErgataiResult;
use tokio::fs;

/// Root of the task tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTree {
    pub root: TaskNode,

    /// Optional: when this tree was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,

    /// Optional: description of the overall goal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A node in the task tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    /// Unique identifier (e.g., "pm", "dev-1", "test-login")
    pub id: String,

    /// Agent name responsible for this task
    pub agent: String,

    /// Human-readable task description
    pub task: String,

    /// Current status
    pub status: TaskStatus,

    /// Sub-tasks (children in the tree)
    #[serde(default)]
    pub children: Vec<TaskNode>,

    /// Optional: path to result file (set when completed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,

    /// Optional: sibling IDs this node can communicate with
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sibling_links: Vec<String>,

    /// Optional: metadata (for AI to attach context)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Task execution status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not started yet (waiting for parent)
    Pending,
    /// Currently being executed
    Running,
    /// Successfully completed
    Completed,
    /// Failed (may retry)
    Failed,
}

impl TaskTree {
    /// Create a new task tree with a root node
    pub fn new(root: TaskNode) -> Self {
        Self {
            root,
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            description: None,
        }
    }

    /// Find all nodes that are ready to execute
    /// (status=Pending and parent is Completed)
    pub fn ready_tasks(&self) -> Vec<&TaskNode> {
        // Pre-allocate with a reasonable default
        let mut ready = Vec::with_capacity(8);
        // Root is always ready if pending (no parent)
        if matches!(self.root.status, TaskStatus::Pending) {
            ready.push(&self.root);
        } else {
            self.collect_ready(&self.root, &mut ready);
        }
        ready
    }

    fn collect_ready<'a>(&'a self, node: &'a TaskNode, ready: &mut Vec<&'a TaskNode>) {
        let parent_done = matches!(node.status, TaskStatus::Completed);
        for child in &node.children {
            if matches!(child.status, TaskStatus::Pending) && parent_done {
                ready.push(child);
            }
            // Recursively check grandchildren
            self.collect_ready(child, ready);
        }
    }

    /// Find a node by ID (DFS)
    pub fn find_node(&self, id: &str) -> Option<&TaskNode> {
        self.find_node_recursive(&self.root, id)
    }

    fn find_node_recursive<'a>(&'a self, node: &'a TaskNode, id: &str) -> Option<&'a TaskNode> {
        if node.id == id {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = self.find_node_recursive(child, id) {
                return Some(found);
            }
        }
        None
    }

    /// Find a mutable node by ID (DFS)
    pub fn find_node_mut(&mut self, id: &str) -> Option<&mut TaskNode> {
        Self::find_node_mut_recursive(&mut self.root, id)
    }

    fn find_node_mut_recursive<'a>(node: &'a mut TaskNode, id: &str) -> Option<&'a mut TaskNode> {
        if node.id == id {
            return Some(node);
        }
        for child in &mut node.children {
            if let Some(found) = Self::find_node_mut_recursive(child, id) {
                return Some(found);
            }
        }
        None
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

    /// Get overall progress (0.0 to 1.0)
    pub fn progress(&self) -> f32 {
        let total = self.count_nodes();
        if total == 0 {
            return 0.0;
        }
        let completed = self.count_completed();
        completed as f32 / total as f32
    }

    fn count_nodes(&self) -> usize {
        self.count_nodes_recursive(&self.root)
    }

    fn count_nodes_recursive(&self, node: &TaskNode) -> usize {
        1 + node.children.iter().map(|c| self.count_nodes_recursive(c)).sum::<usize>()
    }

    fn count_completed(&self) -> usize {
        self.count_completed_recursive(&self.root)
    }

    fn count_completed_recursive(&self, node: &TaskNode) -> usize {
        let this = if matches!(node.status, TaskStatus::Completed) {
            1
        } else {
            0
        };
        this + node
            .children
            .iter()
            .map(|c| self.count_completed_recursive(c))
            .sum::<usize>()
    }

    /// Check if all tasks are completed
    pub fn is_complete(&self) -> bool {
        let total = self.count_nodes();
        total > 0 && self.count_completed() == total
    }

    /// Serialize to AI-friendly format (with visual tree structure)
    pub fn to_ai_prompt(&self) -> String {
        let mut output = String::new();

        if let Some(desc) = &self.description {
            output.push_str(&format!("Goal: {}\n\n", desc));
        }

        output.push_str("Task Tree:\n");
        self.format_node(&self.root, 0, &mut output);

        output.push_str(&format!("\nProgress: {:.0}%\n", self.progress() * 100.0));

        let ready = self.ready_tasks();
        if !ready.is_empty() {
            output.push_str("\nReady to execute:\n");
            for node in ready {
                output.push_str(&format!("  - [{}] {}\n", node.id, node.task));
            }
        }

        output
    }

    fn format_node(&self, node: &TaskNode, depth: usize, output: &mut String) {
        let indent = "  ".repeat(depth);
        let status_icon = match node.status {
            TaskStatus::Pending => "⏳",
            TaskStatus::Running => "🔄",
            TaskStatus::Completed => "✅",
            TaskStatus::Failed => "❌",
        };

        output.push_str(&format!(
            "{}{} [{}] {} → {}\n",
            indent, status_icon, node.id, node.agent, node.task
        ));

        for child in &node.children {
            self.format_node(child, depth + 1, output);
        }
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
        let tree: Self = serde_json::from_str(&content)?;
        Ok(tree)
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
            children: Vec::new(),
            result_path: None,
            sibling_links: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a child task
    pub fn with_child(mut self, child: TaskNode) -> Self {
        self.children.push(child);
        self
    }

    /// Add sibling link (peer communication)
    pub fn with_sibling_link(mut self, sibling_id: impl Into<String>) -> Self {
        self.sibling_links.push(sibling_id.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> TaskTree {
        let root = TaskNode::new("pm", "pm-agent", "Analyze requirements")
            .with_child(
                TaskNode::new("dev-1", "dev-agent", "Implement login")
                    .with_child(TaskNode::new("test-1", "test-agent", "Test login")),
            )
            .with_child(
                TaskNode::new("dev-2", "dev-agent", "Implement registration")
                    .with_child(TaskNode::new("test-2", "test-agent", "Test registration")),
            );

        TaskTree::new(root)
    }

    #[test]
    fn test_ready_tasks_initial() {
        let tree = sample_tree();
        let ready = tree.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "pm");
    }

    #[test]
    fn test_ready_tasks_after_root() {
        let mut tree = sample_tree();
        tree.update_status("pm", TaskStatus::Completed).unwrap();

        let ready = tree.ready_tasks();
        assert_eq!(ready.len(), 2);
        let ids: Vec<_> = ready.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"dev-1"));
        assert!(ids.contains(&"dev-2"));
    }

    #[test]
    fn test_progress() {
        let mut tree = sample_tree();
        assert_eq!(tree.progress(), 0.0);

        tree.update_status("pm", TaskStatus::Completed).unwrap();
        assert!((tree.progress() - 0.2).abs() < 0.01); // 1/5

        tree.update_status("dev-1", TaskStatus::Completed).unwrap();
        assert!((tree.progress() - 0.4).abs() < 0.01); // 2/5
    }

    #[test]
    fn test_find_node() {
        let tree = sample_tree();
        assert!(tree.find_node("pm").is_some());
        assert!(tree.find_node("dev-1").is_some());
        assert!(tree.find_node("test-2").is_some());
        assert!(tree.find_node("nonexistent").is_none());
    }

    // ── Additional topology tests ──

    #[test]
    fn test_single_node_tree() {
        let tree = TaskTree::new(TaskNode::new("root", "agent", "Only task"));
        assert_eq!(tree.root.children.len(), 0);
        let ready = tree.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "root");
        assert_eq!(tree.progress(), 0.0);
        assert!(!tree.is_complete());
    }

    #[test]
    fn test_deep_tree() {
        // Build a 10-level deep tree
        let mut root = TaskNode::new("l0", "agent", "Level 0");
        let mut current = &mut root;
        for i in 1..=10 {
            let child = TaskNode::new(format!("l{}", i), "agent", format!("Level {}", i));
            current.children.push(child);
            current = current.children.last_mut().unwrap();
        }
        let tree = TaskTree::new(root);
        // Only root is ready initially
        assert_eq!(tree.ready_tasks().len(), 1);
        assert_eq!(tree.progress(), 0.0);
        // Total 11 nodes
        let total_nodes = tree.count_nodes();
        assert_eq!(total_nodes, 11);
    }

    #[test]
    fn test_unbalanced_tree() {
        let root = TaskNode::new("root", "agent", "Root")
            .with_child(
                TaskNode::new("heavy", "agent", "Heavy branch")
                    .with_child(TaskNode::new("h1", "agent", "H1"))
                    .with_child(TaskNode::new("h2", "agent", "H2"))
                    .with_child(TaskNode::new("h3", "agent", "H3")),
            )
            .with_child(TaskNode::new("light", "agent", "Light branch"));

        let tree = TaskTree::new(root);
        assert_eq!(tree.count_nodes(), 6); // root + heavy + h1 + h2 + h3 + light
    }

    #[test]
    fn test_wide_tree() {
        let mut root = TaskNode::new("root", "agent", "Root");
        for i in 0..20 {
            root.children.push(TaskNode::new(
                format!("child-{}", i),
                "agent",
                format!("Child {}", i),
            ));
        }
        let tree = TaskTree::new(root);
        assert_eq!(tree.count_nodes(), 21);
        // Complete root, then all children should be ready
        let mut tree = tree;
        tree.update_status("root", TaskStatus::Completed).unwrap();
        let ready = tree.ready_tasks();
        assert_eq!(ready.len(), 20);
    }

    #[test]
    fn test_tree_with_no_leaves() {
        // All nodes have children (except the deepest level)
        // But the "no leaves" scenario is really about internal nodes being complete
        // Here: just test a tree where only the root is pending and no children exist at leaf level
        let root = TaskNode::new("root", "agent", "Root")
            .with_child(TaskNode::new("mid", "agent", "Mid")
                .with_child(TaskNode::new("leaf", "agent", "Leaf")));
        let tree = TaskTree::new(root);
        // Root is the only ready task
        let ready = tree.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "root");
    }

    #[test]
    fn test_update_status_missing_node() {
        let mut tree = sample_tree();
        let result = tree.update_status("nonexistent", TaskStatus::Completed);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_result_sets_completed() {
        let mut tree = sample_tree();
        tree.set_result("pm", "/path/to/result".to_string()).unwrap();
        let node = tree.find_node("pm").unwrap();
        assert_eq!(node.status, TaskStatus::Completed);
        assert_eq!(node.result_path, Some("/path/to/result".to_string()));
    }

    #[test]
    fn test_is_complete_all_completed() {
        let mut tree = sample_tree();
        tree.update_status("pm", TaskStatus::Completed).unwrap();
        tree.update_status("dev-1", TaskStatus::Completed).unwrap();
        tree.update_status("dev-2", TaskStatus::Completed).unwrap();
        tree.update_status("test-1", TaskStatus::Completed).unwrap();
        tree.update_status("test-2", TaskStatus::Completed).unwrap();
        assert!(tree.is_complete());
        assert_eq!(tree.progress(), 1.0);
    }

    #[test]
    fn test_sibling_links() {
        let root = TaskNode::new("pm", "agent", "PM")
            .with_child(
                TaskNode::new("dev-1", "agent", "Dev 1")
                    .with_sibling_link("dev-2"),
            )
            .with_child(
                TaskNode::new("dev-2", "agent", "Dev 2")
                    .with_sibling_link("dev-1"),
            );
        let tree = TaskTree::new(root);
        let dev1 = tree.find_node("dev-1").unwrap();
        assert_eq!(dev1.sibling_links, vec!["dev-2"]);
        let dev2 = tree.find_node("dev-2").unwrap();
        assert_eq!(dev2.sibling_links, vec!["dev-1"]);
    }

    #[test]
    fn test_find_node_mut() {
        let mut tree = sample_tree();
        let node = tree.find_node_mut("dev-1").unwrap();
        node.status = TaskStatus::Running;
        assert_eq!(tree.find_node("dev-1").unwrap().status, TaskStatus::Running);
    }

    #[test]
    fn test_metadata_stored() {
        let mut root = TaskNode::new("root", "agent", "Root");
        root.metadata.insert("key".to_string(), "value".to_string());
        let tree = TaskTree::new(root);
        assert_eq!(
            tree.find_node("root").unwrap().metadata.get("key"),
            Some(&"value".to_string())
        );
    }

    #[test]
    fn test_progress_zero_nodes_after_construction() {
        let tree = sample_tree();
        assert_eq!(tree.progress(), 0.0);
    }

    #[test]
    fn test_ready_tasks_after_partial_completion() {
        let mut tree = sample_tree();
        tree.update_status("pm", TaskStatus::Completed).unwrap();
        tree.update_status("dev-1", TaskStatus::Completed).unwrap();
        // test-1 should be ready now (its parent dev-1 is completed)
        let ready = tree.ready_tasks();
        let ids: Vec<_> = ready.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"test-1"));
        assert!(ids.contains(&"dev-2"));
    }
}
