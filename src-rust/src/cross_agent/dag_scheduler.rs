//! DAG Scheduler - Integrates TaskGraph with TaskScheduler
//!
//! Bridges the DAG-based orchestration with the existing task scheduling system.
//! Main Agent submits a DAG → DagScheduler extracts ready tasks → TaskScheduler executes them
//! → On completion → DagScheduler checks for newly ready nodes → Repeat

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{ErgataiError, ErgataiResult};
use tokio::sync::Mutex;

use super::task_scheduler::{global_scheduler, TaskScheduler};
use crate::orchestration::{TaskGraph, TaskNode, TaskStatus};

/// DAG Scheduler - manages DAG-based task orchestration
#[derive(Clone)]
pub struct DagScheduler {
    /// The task graph being executed
    graph: Arc<Mutex<TaskGraph>>,

    /// Project root for file paths
    project_root: PathBuf,

    /// Reference to the global task scheduler
    scheduler: Arc<TaskScheduler>,
}

impl DagScheduler {
    /// Create a new DAG scheduler
    pub fn new(project_root: PathBuf, graph: TaskGraph) -> Self {
        Self {
            graph: Arc::new(Mutex::new(graph)),
            project_root: project_root.clone(),
            scheduler: global_scheduler(Some(project_root)),
        }
    }

    /// Submit the DAG for execution
    /// Extracts all ready tasks and submits them to the scheduler
    pub async fn submit_graph(&self) -> ErgataiResult<Vec<String>> {
        // Collect ready nodes while holding lock, then release it
        let ready_nodes: Vec<TaskNode> = {
            let graph = self.graph.lock().await;
            graph.ready_tasks().into_iter().cloned().collect()
        };

        let mut submitted = Vec::new();
        for node in ready_nodes {
            match self.generate_and_submit(&node).await {
                Ok(task_id) => {
                    // Update status after successful submission
                    let mut graph = self.graph.lock().await;
                    graph.update_status(&node.id, TaskStatus::Running)?;
                    drop(graph);

                    tracing::info!("Submitted node {} as task {}", node.id, task_id);
                    submitted.push(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to submit node {}: {}", node.id, e);
                }
            }
        }

        // Save graph state — serialize under lock, write without
        self.save_graph_unlocked().await?;

        Ok(submitted)
    }

    /// Generate plan and submit to scheduler (no lock acquisition)
    async fn generate_and_submit(&self, node: &TaskNode) -> ErgataiResult<String> {
        // Release lock before async I/O — generate_node_plan doesn't need the graph
        // (ponytail: held lock across await made future !Send, breaking tokio::spawn callers)
        {
            let _graph = self.graph.lock().await;
            // Lock intentionally dropped here before async work below
        }

        // Generate a simple plan file for this node
        let plan_file = self.generate_node_plan(node).await?;

        // Submit to scheduler
        let task_id = self.scheduler.submit_task(plan_file).await?;

        Ok(task_id)
    }

    /// Generate a plan file for a single node
    /// This reuses the existing plan file infrastructure
    async fn generate_node_plan(&self, node: &TaskNode) -> ErgataiResult<PathBuf> {
        let plan_dir = self.project_root.join(".ergatai").join(".dag-plans");
        tokio::fs::create_dir_all(&plan_dir).await?;

        let plan_file = plan_dir.join(format!("{}.md", node.id));

        // Generate simple plan content
        let content = format!(
            r#"# Task: {}

### @{} - {}
- **Objective**: {}
- **Type**: {}
- **Result**: .ergatai/.dag-results/{}.md
"#,
            node.task,
            node.agent,
            node.id,
            node.task,
            "CreateNew", // Default task type
            node.id
        );

        tokio::fs::write(&plan_file, content).await?;

        // Create results directory
        let results_dir = self.project_root.join(".ergatai").join(".dag-results");
        tokio::fs::create_dir_all(&results_dir).await?;

        Ok(plan_file)
    }

    /// Called when a node completes
    /// Checks for newly ready nodes and submits them
    pub async fn on_node_completed(
        &self,
        node_id: &str,
        result_path: Option<String>,
    ) -> ErgataiResult<Vec<String>> {
        // Update status under lock, then release
        {
            let mut graph = self.graph.lock().await;
            if let Some(result) = result_path {
                graph.set_result(node_id, result)?;
            } else {
                graph.update_status(node_id, TaskStatus::Completed)?;
            }
        }

        tracing::info!(
            "Node {} completed, checking for newly ready nodes",
            node_id
        );

        // Collect newly ready nodes while holding lock
        let ready_nodes: Vec<TaskNode> = {
            let graph = self.graph.lock().await;
            graph
                .ready_tasks()
                .into_iter()
                .filter(|n| n.status == TaskStatus::Pending)
                .cloned()
                .collect()
        };

        let mut newly_submitted = Vec::new();
        for node in ready_nodes {
            match self.generate_and_submit(&node).await {
                Ok(task_id) => {
                    // Update status after successful submission
                    let mut graph = self.graph.lock().await;
                    graph.update_status(&node.id, TaskStatus::Running)?;
                    drop(graph);

                    tracing::info!(
                        "Submitted newly ready node {} as task {}",
                        node.id,
                        task_id
                    );
                    newly_submitted.push(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to submit node {}: {}", node.id, e);
                }
            }
        }

        // Save updated graph — serialize under lock, then write without holding it
        let graph_json = {
            let graph = self.graph.lock().await;
            serde_json::to_string(&*graph).map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))?
        };
        let graph_file = self.project_root.join(".ergatai").join("dag-state.json");
        tokio::fs::write(&graph_file, graph_json.as_bytes()).await?;

        // Check if all done
        let is_done = {
            let graph = self.graph.lock().await;
            graph.is_complete()
        };
        if is_done {
            tracing::info!("All nodes completed! DAG execution complete.");
        }

        Ok(newly_submitted)
    }

    /// Called when a node fails
    pub async fn on_node_failed(&self, node_id: &str, error: &str) -> ErgataiResult<()> {
        // Determine retry decision and clone node data under lock
        let (should_retry, retry_count, node_clone) = {
            let mut graph = self.graph.lock().await;

            let should_retry = graph.retry_failed(node_id)?;
            let node = graph
                .find_node(node_id)
                .ok_or_else(|| ErgataiError::InvalidArgument(format!("Node not found: {}", node_id)))?;
            (should_retry, node.retry_count, node.clone())
        }; // Lock released here

        if should_retry {
            tracing::info!("Node {} failed, retrying (attempt {})", node_id, retry_count);

            // Submit without holding lock
            match self.generate_and_submit(&node_clone).await {
                Ok(_task_id) => {
                    let mut graph = self.graph.lock().await;
                    graph.update_status(node_id, TaskStatus::Running)?;
                }
                Err(e) => {
                    tracing::error!("Failed to retry node {}: {}", node_id, e);
                    let mut graph = self.graph.lock().await;
                    graph.update_status(node_id, TaskStatus::Failed)?;
                    drop(graph);
                    self.save_graph_unlocked().await?;
                }
            }
        } else {
            tracing::error!("Node {} failed: {} (no more retries)", node_id, error);
            {
                let mut graph = self.graph.lock().await;
                graph.update_status(node_id, TaskStatus::Failed)?;
            }

            // Propagate failure: skip all downstream nodes
            self.skip_downstream(node_id).await?;

            self.save_graph_unlocked().await?;
        }

        Ok(())
    }

    /// Skip all nodes that (transitively) depend on the failed node.
    ///
    /// ponytail: BFS under a single read lock to collect all skip targets,
    /// then batch-update under a single write lock. Avoids the recursive
    /// lock/unlock cycles the old implementation had.
    async fn skip_downstream(&self, failed_id: &str) -> ErgataiResult<()> {
        // 1. BFS to collect all transitively dependent pending nodes.
        let to_skip: Vec<String> = {
            let graph = self.graph.lock().await;
            let mut queue = vec![failed_id.to_string()];
            let mut to_skip = Vec::new();
            let mut seen = std::collections::HashSet::new();  // O(1) lookup

            while let Some(current) = queue.pop() {
                for node in &graph.nodes {
                    if node.depends_on.contains(&current)
                        && node.status == TaskStatus::Pending
                        && seen.insert(&node.id)  // O(1) check + insert
                    {
                        to_skip.push(node.id.clone());
                        queue.push(node.id.clone());
                    }
                }
            }

            to_skip
        };

        // 2. Batch-update all skipped nodes under a single write lock.
        if !to_skip.is_empty() {
            let mut graph = self.graph.lock().await;
            for node_id in &to_skip {
                if let Some(node) = graph.find_node_mut(node_id) {
                    node.status = TaskStatus::Skipped;
                    tracing::info!("Skipped node {} (depends on failed {})", node_id, failed_id);
                }
            }
        }

        Ok(())
    }

    /// Get current progress
    pub async fn progress(&self) -> f32 {
        let graph = self.graph.lock().await;
        graph.progress()
    }

    /// Get graph status as AI-friendly text
    pub async fn status_prompt(&self) -> String {
        let graph = self.graph.lock().await;
        graph.to_ai_prompt()
    }

    /// Check if all nodes are complete
    pub async fn is_complete(&self) -> bool {
        let graph = self.graph.lock().await;
        graph.is_complete()
    }

    /// Save graph to disk (serializes under lock, writes without holding it)
    /// ponytail: avoids holding MutexGuard across async I/O which would make the future !Send
    async fn save_graph_unlocked(&self) -> ErgataiResult<()> {
        let graph_json = {
            let graph = self.graph.lock().await;
            serde_json::to_string(&*graph).map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))?
        };
        let graph_file = self.project_root.join(".ergatai").join("dag-state.json");
        tokio::fs::write(&graph_file, graph_json.as_bytes()).await?;
        Ok(())
    }

    /// Load graph from disk (for recovery)
    pub async fn load_from_disk(project_root: PathBuf) -> ErgataiResult<Self> {
        let graph_file = project_root.join(".ergatai").join("dag-state.json");
        let graph = TaskGraph::load_from_file(&graph_file).await?;
        Ok(Self::new(project_root, graph))
    }

    /// Get a JSON snapshot of the current graph state
    pub async fn graph_snapshot(&self) -> ErgataiResult<String> {
        let graph = self.graph.lock().await;
        serde_json::to_string(&*graph).map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))
    }
}

// ── Global DAG Scheduler Singleton ──

use std::sync::Mutex as StdMutex;

static GLOBAL_DAG: std::sync::OnceLock<StdMutex<Option<DagScheduler>>> = std::sync::OnceLock::new();

fn dag_slot() -> &'static StdMutex<Option<DagScheduler>> {
    GLOBAL_DAG.get_or_init(|| StdMutex::new(None))
}

/// Set the active DAG scheduler (replaces any existing one)
pub fn set_dag_scheduler(scheduler: DagScheduler) {
    match dag_slot().lock() {
        Ok(mut guard) => *guard = Some(scheduler),
        Err(poisoned) => {
            tracing::error!("Global DAG scheduler lock poisoned, recovering");
            *poisoned.into_inner() = Some(scheduler);
        }
    }
}

/// Get a clone of the active DAG scheduler, if any
pub fn get_dag_scheduler() -> Option<DagScheduler> {
    match dag_slot().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            tracing::error!("Global DAG scheduler lock poisoned, recovering");
            poisoned.into_inner().clone()
        }
    }
}

/// Clear the active DAG scheduler
pub fn clear_dag_scheduler() {
    match dag_slot().lock() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => {
            tracing::error!("Global DAG scheduler lock poisoned, recovering");
            *poisoned.into_inner() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::TaskNode;

    fn sample_graph() -> TaskGraph {
        TaskGraph::new(vec![
            TaskNode::new("n1", "agent-a", "Task A"),
            TaskNode::new("n2", "agent-b", "Task B")
                .with_dependencies(vec!["n1".into()]),
        ])
    }

    #[tokio::test]
    async fn test_dag_scheduler_creation() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp"), graph);
        assert!(!scheduler.is_complete().await);
    }

    #[tokio::test]
    async fn test_progress_tracking() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp"), graph);
        assert_eq!(scheduler.progress().await, 0.0);
    }

    #[tokio::test]
    async fn test_on_node_failed_marks_downstream_skipped() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent", "Task 1"),
            TaskNode::new("n2", "agent", "Task 2").with_dependencies(vec!["n1".into()]),
            TaskNode::new("n3", "agent", "Task 3").with_dependencies(vec!["n2".into()]),
        ]);

        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".ergatai")).unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);

        // Mark n1 as failed (no retries configured)
        scheduler.on_node_failed("n1", "boom").await.unwrap();

        // Check that n1 is Failed and n2, n3 are Skipped
        let graph = scheduler.graph.lock().await;
        assert_eq!(graph.find_node("n1").unwrap().status, TaskStatus::Failed);
        assert_eq!(graph.find_node("n2").unwrap().status, TaskStatus::Skipped);
        assert_eq!(graph.find_node("n3").unwrap().status, TaskStatus::Skipped);
    }

    #[tokio::test]
    async fn test_global_dag_scheduler_lifecycle() {
        // Clean slate
        clear_dag_scheduler();
        assert!(get_dag_scheduler().is_none());

        // Set
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp"), graph);
        set_dag_scheduler(scheduler);

        // Get
        let retrieved = get_dag_scheduler();
        assert!(retrieved.is_some());
        assert!(!retrieved.unwrap().is_complete().await);

        // Clear
        clear_dag_scheduler();
        assert!(get_dag_scheduler().is_none());
    }

    #[tokio::test]
    async fn test_global_dag_scheduler_replace() {
        clear_dag_scheduler();

        // Set first scheduler
        let graph1 = sample_graph();
        set_dag_scheduler(DagScheduler::new(PathBuf::from("/tmp"), graph1));
        assert!(get_dag_scheduler().is_some());

        // Replace with second scheduler
        let graph2 = TaskGraph::new(vec![
            TaskNode::new("x1", "agent", "Task X"),
        ]);
        set_dag_scheduler(DagScheduler::new(PathBuf::from("/tmp"), graph2));

        // Should have the new one
        let s = get_dag_scheduler().unwrap();
        assert_eq!(s.progress().await, 0.0);

        clear_dag_scheduler();
    }
}


