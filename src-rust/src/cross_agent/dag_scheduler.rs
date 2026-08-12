//! DAG Scheduler - Integrates TaskGraph with TaskScheduler
//!
//! Bridges the DAG-based orchestration with the existing task scheduling system.
//! Main Agent submits a DAG → DagScheduler extracts ready tasks → TaskScheduler executes them
//! → On completion → DagScheduler checks for newly ready nodes → Repeat

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{ErgataiError, ErgataiResult};
use crate::orchestration::context::DagContext;
use tokio::sync::Mutex;

use super::task_scheduler::{global_scheduler, TaskScheduler};
use crate::orchestration::{TaskGraph, TaskNode, TaskStatus};

/// DAG Scheduler - manages DAG-based task orchestration
#[derive(Clone)]
pub struct DagScheduler {
    /// The task graph being executed
    graph: Arc<Mutex<TaskGraph>>,

    /// Execution context (global vars + per-node outputs) for template rendering
    context: Arc<Mutex<DagContext>>,

    /// Project root for file paths
    project_root: PathBuf,

    /// Reference to the global task scheduler
    scheduler: Arc<TaskScheduler>,
}

impl DagScheduler {
    /// Create a new DAG scheduler with an empty context
    pub fn new(project_root: PathBuf, graph: TaskGraph) -> Self {
        Self::with_context(project_root, graph, DagContext::empty())
    }

    /// Create a new DAG scheduler with the given context
    pub fn with_context(
        project_root: PathBuf,
        graph: TaskGraph,
        context: DagContext,
    ) -> Self {
        Self {
            graph: Arc::new(Mutex::new(graph)),
            context: Arc::new(Mutex::new(context)),
            project_root: project_root.clone(),
            scheduler: global_scheduler(Some(project_root)),
        }
    }

    /// Get a clone of the execution context
    pub fn context(&self) -> Arc<Mutex<DagContext>> {
        self.context.clone()
    }

    /// Set a global variable in the context
    pub async fn set_global(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut ctx = self.context.lock().await;
        ctx.set_global(key, value);
    }

    /// Record outputs from a completed node into the context
    pub async fn record_outputs(
        &self,
        node_id: &str,
        outputs: HashMap<String, String>,
    ) {
        let mut ctx = self.context.lock().await;
        ctx.record_output(node_id, outputs);
    }

    /// Submit the DAG for execution
    /// Extracts all ready tasks and submits them to the scheduler
    pub async fn submit_graph(&self) -> ErgataiResult<Vec<String>> {
        // Clear completed/failed agents from previous DAG runs (M14 fix)
        let launcher = super::agent_launcher::AgentLauncher::new(self.project_root.clone());
        launcher.clear_stale_agents().await?;

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
    ///
    /// Prefers NATS event publishing when available (decoupled, event-driven).
    /// Falls back to direct `task_scheduler.submit_task()` call otherwise.
    async fn generate_and_submit(&self, node: &TaskNode) -> ErgataiResult<String> {
        // Generate plan file (still needed — agents read it as a document)
        let plan_file = self.generate_node_plan(node).await?;
        let task_id = node.id.clone();

        if crate::nats::is_nats_initialized().await {
            // NATS path: publish task submission event with inline plan content
            if let Some(conn) = crate::nats::get_nats_connection().await {
                let bus = crate::nats::EventBus::new(conn);
                let plan_content = tokio::fs::read_to_string(&plan_file).await?;
                let dag_id = self.dag_id();

                let payload = crate::nats::TaskSubmitPayload {
                    task_id: task_id.clone(),
                    plan_content,
                    plan_file: plan_file.to_string_lossy().to_string(),
                    target_agent: node.agent.clone(),
                    priority: crate::file_access::conflict_arbitration::priority_to_number(&node.priority)
                        .map(|p| p as u32)
                        .unwrap_or(2),
                    timeout_secs: node.timeout,
                    dag_id: Some(dag_id),
                };

                bus.publish_task_submit(&payload).await?;
                tracing::info!(task_id = task_id, "Submitted node via NATS event");
                return Ok(task_id);
            }
        }

        // Fallback: direct task_scheduler call
        let tid = self.scheduler.submit_task(plan_file).await?;
        Ok(tid)
    }

    /// Get a DAG identifier (derived from project root)
    fn dag_id(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.project_root.hash(&mut hasher);
        format!("dag-{:x}", hasher.finish())
    }

    /// Start listening for NATS DAG events (node_complete, node_failed)
    ///
    /// Spawns a background task that subscribes to:
    /// - `ergatai.dag.node_complete.*` — triggers `on_node_completed()`
    /// - `ergatai.dag.node_failed.*`   — triggers `on_node_failed()`
    ///
    /// Returns a `JoinHandle` that can be aborted to stop listening.
    pub fn start_event_listener(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let conn = match crate::nats::get_nats_connection().await {
                Some(c) => c,
                None => {
                    tracing::warn!("NATS not initialized, event listener not started");
                    return;
                }
            };

            let bus = crate::nats::EventBus::new(conn);

            // Subscribe to all node completion events
            let mut complete_sub = match bus.subscribe_all_node_complete().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to subscribe to node_complete events");
                    return;
                }
            };

            // Subscribe to all node failure events
            let mut failed_sub = match bus.subscribe_all_node_failed().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to subscribe to node_failed events");
                    return;
                }
            };

            tracing::info!("DAG event listener started");

            use futures_util::StreamExt;
            loop {
                tokio::select! {
                    // Handle node completion
                    msg = complete_sub.next() => {
                        match msg {
                            Some(nats_msg) => {
                                match serde_json::from_slice::<crate::nats::NodeCompletePayload>(&nats_msg.payload) {
                                    Ok(payload) => {
                                        tracing::info!(
                                            node_id = %payload.node_id,
                                            "Received NATS node_complete event"
                                        );
                                        // Record outputs into context
                                        if !payload.outputs.is_empty() {
                                            self.record_outputs(&payload.node_id, payload.outputs).await;
                                        }
                                        // Trigger downstream nodes
                                        match self.on_node_completed(&payload.node_id, payload.result_file).await {
                                            Ok(newly_submitted) => {
                                                tracing::info!(
                                                    node_id = %payload.node_id,
                                                    newly_submitted = newly_submitted.len(),
                                                    "Processed node_complete, submitted downstream"
                                                );
                                            }
                                            Err(e) => {
                                                tracing::error!(
                                                    node_id = %payload.node_id,
                                                    error = %e,
                                                    "Failed to process node_complete"
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "Failed to deserialize node_complete event");
                                    }
                                }
                            }
                            None => {
                                tracing::warn!("node_complete subscription closed");
                                break;
                            }
                        }
                    }

                    // Handle node failure
                    msg = failed_sub.next() => {
                        match msg {
                            Some(nats_msg) => {
                                match serde_json::from_slice::<crate::nats::NodeFailedPayload>(&nats_msg.payload) {
                                    Ok(payload) => {
                                        tracing::info!(
                                            node_id = %payload.node_id,
                                            error = %payload.error,
                                            "Received NATS node_failed event"
                                        );
                                        match self.on_node_failed(&payload.node_id, &payload.error).await {
                                            Ok(()) => {
                                                tracing::info!(
                                                    node_id = %payload.node_id,
                                                    "Processed node_failed"
                                                );
                                            }
                                            Err(e) => {
                                                tracing::error!(
                                                    node_id = %payload.node_id,
                                                    error = %e,
                                                    "Failed to process node_failed"
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "Failed to deserialize node_failed event");
                                    }
                                }
                            }
                            None => {
                                tracing::warn!("node_failed subscription closed");
                                break;
                            }
                        }
                    }
                }
            }

            tracing::info!("DAG event listener stopped");
        })
    }

    /// Generate a plan file for a single node
    ///
    /// This is where data flow happens: we read the task document (if present),
    /// render all `{{var}}` templates against the current `DagContext` (global
    /// vars + upstream outputs), and include upstream dependency context so the
    /// agent can see what previous nodes produced.
    async fn generate_node_plan(&self, node: &TaskNode) -> ErgataiResult<PathBuf> {
        let plan_dir = self.project_root.join(".ergatai").join(".dag-plans");
        tokio::fs::create_dir_all(&plan_dir).await?;

        let plan_file = plan_dir.join(format!("{}.md", node.id));

        // 1. Render the node's input template (if any)
        let rendered_input = if let Some(ref input_tmpl) = node.input {
            let ctx = self.context.lock().await;
            Some(ctx.render_template(input_tmpl))
        } else {
            None
        };

        // 2. Read upstream dependency outputs from the context
        let upstream_context = self.build_upstream_context_block(node).await;

        // 3. Build the plan content
        let result_path = format!(".ergatai/.dag-results/{}.md", node.id);
        let content = format!(
            r#"# Task: {}

### @{} - {}
- **Objective**: {}
- **Type**: {}
- **Result**: {}
{}
{}
"#,
            node.task,
            node.agent,
            node.id,
            node.task,
            "CreateNew",
            result_path,
            rendered_input
                .as_ref()
                .map(|s| format!("- **Input**: {}", s))
                .unwrap_or_default(),
            upstream_context,
        );

        tokio::fs::write(&plan_file, content).await?;

        // Create results directory
        let results_dir = self.project_root.join(".ergatai").join(".dag-results");
        tokio::fs::create_dir_all(&results_dir).await?;

        Ok(plan_file)
    }

    /// Build a markdown block describing upstream node outputs
    ///
    /// For each completed dependency, render a section showing what keys it produced.
    async fn build_upstream_context_block(&self, node: &TaskNode) -> String {
        if node.depends_on.is_empty() {
            return String::new();
        }

        let graph = self.graph.lock().await;
        let ctx = self.context.lock().await;

        let mut lines = Vec::new();
        lines.push(String::new());
        lines.push("### Upstream Context".to_string());

        for dep_id in &node.depends_on {
            // Find the dependency node to get its human-readable name
            let dep_name = graph
                .find_node(dep_id)
                .map(|n| n.task.as_str())
                .unwrap_or(dep_id);

            if let Some(outputs) = ctx.get_node_outputs(dep_id) {
                if !outputs.is_empty() {
                    lines.push(format!("\n**{}** ({}) outputs:", dep_name, dep_id));
                    for (k, v) in outputs {
                        lines.push(format!("  - {}: {}", k, v));
                    }
                } else {
                    lines.push(format!("\n**{}** ({}) — completed (no outputs recorded)", dep_name, dep_id));
                }
            } else {
                lines.push(format!("\n**{}** ({}) — completed", dep_name, dep_id));
            }
        }

        lines.join("\n")
    }

    /// Called when a node completes
    /// Checks for newly ready nodes and submits them
    pub async fn on_node_completed(
        &self,
        node_id: &str,
        result_path: Option<String>,
    ) -> ErgataiResult<Vec<String>> {
        // Update completed node status AND atomically preempt ready nodes as Running
        // within a single lock acquisition to prevent TOCTOU duplicate submission.
        let ready_nodes: Vec<TaskNode> = {
            let mut graph = self.graph.lock().await;
            if let Some(result) = result_path {
                graph.set_result(node_id, result)?;
            } else {
                graph.update_status(node_id, TaskStatus::Completed)?;
            }

            // Collect and immediately preempt pending ready nodes as Running.
            // This prevents concurrent on_node_completed calls from submitting
            // the same node twice.
            let ready: Vec<TaskNode> = graph
                .ready_tasks()
                .into_iter()
                .filter(|n| n.status == TaskStatus::Pending)
                .cloned()
                .collect();
            for n in &ready {
                graph.update_status(&n.id, TaskStatus::Running)?;
            }
            ready
        };

        tracing::info!(
            "Node {} completed, {} newly ready nodes preempted",
            node_id,
            ready_nodes.len()
        );

        let mut newly_submitted = Vec::new();
        for node in ready_nodes {
            match self.generate_and_submit(&node).await {
                Ok(task_id) => {
                    tracing::info!(
                        "Submitted newly ready node {} as task {}",
                        node.id,
                        task_id
                    );
                    newly_submitted.push(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to submit node {}: {}", node.id, e);
                    // Revert status so the node can be retried
                    let mut graph = self.graph.lock().await;
                    let _ = graph.update_status(&node.id, TaskStatus::Pending);
                }
            }
        }

        // Save graph + context together
        self.save_graph_unlocked().await?;

        // Check if all done
        let is_done = {
            let graph = self.graph.lock().await;
            graph.is_complete()
        };
        if is_done {
            tracing::info!("All nodes completed! DAG execution complete.");

            // Publish DAG completion event via NATS
            if crate::nats::is_nats_initialized().await {
                if let Some(conn) = crate::nats::get_nats_connection().await {
                    let bus = crate::nats::EventBus::new(conn);
                    let graph = self.graph.lock().await;
                    let total = graph.nodes.len() as u32;
                    let (completed, failed) = graph.nodes.iter().fold(
                        (0u32, 0u32),
                        |(c, f), n| match n.status {
                            TaskStatus::Completed => (c + 1, f),
                            TaskStatus::Failed => (c, f + 1),
                            _ => (c, f),
                        },
                    );
                    drop(graph);

                    let payload = crate::nats::DagCompletePayload {
                        dag_id: self.dag_id(),
                        total_nodes: total,
                        completed_nodes: completed,
                        failed_nodes: failed,
                        duration_secs: 0, // TODO: track start time
                    };
                    if let Err(e) = bus.publish_dag_complete(&payload).await {
                        tracing::error!(error = %e, "Failed to publish DAG complete event");
                    }
                }
            }
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

    /// Save graph and context to disk (serializes under lock, writes without holding it)
    async fn save_graph_unlocked(&self) -> ErgataiResult<()> {
        let ergatai_dir = self.project_root.join(".ergatai");

        // Serialize graph
        let graph_json = {
            let graph = self.graph.lock().await;
            serde_json::to_string(&*graph).map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))?
        };
        let graph_file = ergatai_dir.join("dag-state.json");
        tokio::fs::write(&graph_file, graph_json.as_bytes()).await?;

        // Serialize context
        let context_json = {
            let ctx = self.context.lock().await;
            serde_json::to_string(&*ctx).map_err(|e| ErgataiError::json_with_source("Failed to serialize context", e))?
        };
        let context_file = ergatai_dir.join("dag-context.json");
        tokio::fs::write(&context_file, context_json.as_bytes()).await?;

        Ok(())
    }

    /// Load graph and context from disk (for recovery)
    pub async fn load_from_disk(project_root: PathBuf) -> ErgataiResult<Self> {
        let graph_file = project_root.join(".ergatai").join("dag-state.json");
        let graph = TaskGraph::load_from_file(&graph_file).await?;

        // Restore context if available (it may not exist in older DAGs)
        let context_file = project_root.join(".ergatai").join("dag-context.json");
        let context = if context_file.exists() {
            DagContext::load_from_file(&context_file).await?
        } else {
            DagContext::empty()
        };

        Ok(Self::with_context(project_root, graph, context))
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

    /// Integration test: verify end-to-end data flow through template rendering.
    /// A → B (B depends on A), with B's input referencing A's output and a global var.
    #[tokio::test]
    async fn test_data_flow_template_rendering() {
        // Setup: 2-node DAG with B depending on A
        let mut node_a = TaskNode::new("n1", "agent-a", "Review code");
        let _ = &mut node_a; // use it
        let node_b = TaskNode::new("n2", "agent-b", "Fix issues")
            .with_dependencies(vec!["n1".into()])
            .with_input("Fix issues found in review: {{n1.review_result}}. Query: {{global.user_query}}");

        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "agent-a", "Review code"),
            node_b,
        ]);

        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = DagContext::new({
            let mut m = HashMap::new();
            m.insert("user_query".to_string(), "improve performance".to_string());
            m
        });

        let scheduler = DagScheduler::with_context(temp_dir.path().to_path_buf(), graph, ctx);

        // Simulate: node n1 completes with outputs
        let mut outputs = HashMap::new();
        outputs.insert("review_result".to_string(), "3 issues found: unused imports".to_string());
        scheduler.record_outputs("n1", outputs).await;

        // Now generate plan for n2 and verify template was rendered
        let graph = scheduler.graph.lock().await;
        let n2 = graph.find_node("n2").unwrap().clone();
        drop(graph);

        let plan_file = scheduler.generate_node_plan(&n2).await.unwrap();
        let plan_content = tokio::fs::read_to_string(&plan_file).await.unwrap();

        // The plan should contain the RESOLVED values, not the raw templates
        assert!(
            plan_content.contains("3 issues found: unused imports"),
            "Plan should contain rendered upstream output, got:\n{}",
            plan_content
        );
        assert!(
            plan_content.contains("improve performance"),
            "Plan should contain rendered global var, got:\n{}",
            plan_content
        );
        assert!(
            !plan_content.contains("{{n1.review_result}}"),
            "Plan should NOT contain unresolved template"
        );
        assert!(
            !plan_content.contains("{{global.user_query}}"),
            "Plan should NOT contain unresolved template"
        );

        // Upstream context block should show n1's outputs
        assert!(
            plan_content.contains("Upstream Context"),
            "Plan should include upstream context section"
        );
        assert!(
            plan_content.contains("review_result"),
            "Upstream context should list output keys"
        );
    }
}


