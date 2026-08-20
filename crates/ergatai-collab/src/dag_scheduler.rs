//! DAG Scheduler - Integrates TaskGraph with TaskScheduler
//!
//! Bridges the DAG-based orchestration with the existing task scheduling system.
//! Main Agent submits a DAG → DagScheduler extracts ready tasks → TaskScheduler executes them
//! → On completion → DagScheduler checks for newly ready nodes → Repeat

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use ergatai_dag::context::DagContext;
use ergatai_error::{ErgataiError, ErgataiResult};
use tokio::sync::Mutex;

use super::task_scheduler::{global_scheduler, TaskScheduler};
use ergatai_dag::{TaskGraph, TaskNode, TaskStatus};

use crate::collaboration::{CollaborationSession, CommunicationCheck, MeshPolicy};

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

    /// Unique DAG identifier (UUID, generated at construction)
    dag_id: String,

    /// DAG creation timestamp (for duration tracking)
    created_at: std::time::Instant,

    /// Active timeout watchdog handles (node_id → JoinHandle)
    timeout_watchers: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,

    /// DAG-level deadline (if timeout is set). When elapsed, all remaining nodes are failed.
    deadline: Option<std::time::Instant>,

    /// DAG-level timeout watchdog handle
    dag_timeout_watcher: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,

    /// Collaboration session bound to this DAG (participants + mesh policy).
    collaboration: Arc<Mutex<CollaborationSession>>,

    /// Guard against duplicate `finalize_if_terminal` runs when concurrent
    /// node completions race past the `is_complete()` check. The first caller
    /// to `swap(true)` proceeds with the NATS publish + registry cleanup;
    /// subsequent callers return early.
    finalized: Arc<AtomicBool>,

    /// Wall-clock of last observable progress (node completed, failed, or new submit).
    last_progress: Arc<Mutex<std::time::Instant>>,

    /// Monotonic counter of agent invocations (generate_and_submit calls).
    agent_call_count: Arc<AtomicU64>,
    /// Cap on agent invocations, from TaskGraph.max_agent_calls.
    max_agent_calls: Option<u64>,
}

impl DagScheduler {
    /// Create a new DAG scheduler with an empty context
    pub fn new(project_root: PathBuf, graph: TaskGraph) -> Self {
        // Initialize context with parameters from graph
        let context = DagContext::with_parameters(HashMap::new(), graph.parameters.clone());
        Self::with_context(project_root, graph, context)
    }

    /// Create a new DAG scheduler with the given context
    pub fn with_context(project_root: PathBuf, graph: TaskGraph, context: DagContext) -> Self {
        // Use persisted dag_id if available (for recovery), otherwise generate new one
        let dag_id = graph
            .dag_id
            .clone()
            .unwrap_or_else(|| format!("dag-{}", uuid::Uuid::new_v4()));

        // Restore deadline from persisted started_at + timeout
        let deadline = if let (Some(ref started_at), Some(timeout)) =
            (&graph.started_at, graph.timeout)
        {
            // Parse RFC3339 timestamp and calculate remaining deadline
            if let Ok(start_time) = chrono::DateTime::parse_from_rfc3339(started_at) {
                let start_utc = start_time.with_timezone(&chrono::Utc);
                let elapsed = chrono::Utc::now() - start_utc;
                let timeout_duration = std::time::Duration::from_secs(timeout);
                let elapsed_duration = std::time::Duration::from_secs(elapsed.num_seconds() as u64);
                if elapsed_duration < timeout_duration {
                    Some(std::time::Instant::now() + (timeout_duration - elapsed_duration))
                } else {
                    // Already expired
                    Some(std::time::Instant::now())
                }
            } else {
                // Fallback: treat as fresh start
                Some(std::time::Instant::now() + std::time::Duration::from_secs(timeout))
            }
        } else {
            graph
                .timeout
                .map(|timeout| std::time::Instant::now() + std::time::Duration::from_secs(timeout))
        };

        // Build the collaboration session from the graph before moving it into the Arc.
        let policy = graph
            .communication
            .as_deref()
            .map(|s| match MeshPolicy::parse(s) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        communication = %s,
                        error = %e,
                        dag_id = %dag_id,
                        "Invalid communication policy; falling back to Open"
                    );
                    MeshPolicy::Open
                }
            })
            .unwrap_or_default();
        let collaboration = CollaborationSession::from_graph(&dag_id, &graph, policy);
        // Read max_agent_calls before moving graph into the Arc.
        let max_agent_calls = graph.max_agent_calls;

        Self {
            graph: Arc::new(Mutex::new(graph)),
            context: Arc::new(Mutex::new(context)),
            project_root: project_root.clone(),
            scheduler: global_scheduler(Some(project_root)),
            dag_id,
            created_at: std::time::Instant::now(),
            timeout_watchers: Arc::new(Mutex::new(HashMap::new())),
            deadline,
            dag_timeout_watcher: Arc::new(Mutex::new(None)),
            collaboration: Arc::new(Mutex::new(collaboration)),
            finalized: Arc::new(AtomicBool::new(false)),
            last_progress: Arc::new(Mutex::new(std::time::Instant::now())),
            agent_call_count: Arc::new(AtomicU64::new(0)),
            max_agent_calls,
        }
    }

    /// Get a clone of the execution context
    pub fn context(&self) -> Arc<Mutex<DagContext>> {
        self.context.clone()
    }

    /// Get a snapshot of the collaboration session bound to this DAG.
    pub async fn collaboration(&self) -> CollaborationSession {
        self.collaboration.lock().await.clone()
    }

    /// Check whether a message from `from_agent` to `to_agent` is permitted
    /// under this DAG's `MeshPolicy`.
    ///
    /// # Semantics
    ///
    /// - If **both** agents are participants in this DAG's collaboration
    ///   session, the configured `MeshPolicy` is applied. Returns
    ///   `CommunicationCheck::Denied(reason)` when the policy rejects the pair.
    /// - If **either** agent is not a participant (e.g., an agent outside this
    ///   DAG, or a DAG-external pane), the check is skipped and `Allowed` is
    ///   returned — this preserves backward compatibility and avoids breaking
    ///   cross-DAG or out-of-band conversations.
    pub async fn check_communication(
        &self,
        from_agent: &str,
        to_agent: &str,
    ) -> CommunicationCheck {
        // Clone what we need under a single lock, then release it.
        // No need to acquire the graph lock — adjacency is precomputed.
        let (allowed, dag_id, policy_desc) = {
            let session = self.collaboration.lock().await;
            // Only enforce the policy when both endpoints are participants.
            if !session.participants.contains(from_agent)
                || !session.participants.contains(to_agent)
            {
                return CommunicationCheck::NotApplicable;
            }
            (
                session.allows(from_agent, to_agent),
                session.dag_id.clone(),
                format!("{:?}", session.policy),
            )
        };
        if allowed {
            CommunicationCheck::Allowed
        } else {
            CommunicationCheck::Denied(format!(
                "MeshPolicy {} of DAG {} denies message {} → {}",
                policy_desc, dag_id, from_agent, to_agent
            ))
        }
    }

    /// Set a global variable in the context
    pub async fn set_global(&self, key: impl Into<String>, value: impl Into<String>) {
        let mut ctx = self.context.lock().await;
        ctx.set_global(key, value);
    }

    /// Record outputs from a completed node into the context
    pub async fn record_outputs(&self, node_id: &str, outputs: serde_json::Value) {
        let mut ctx = self.context.lock().await;
        ctx.record_output(node_id, outputs);
    }

    /// Submit the DAG for execution
    /// Extracts all ready tasks and submits them to the scheduler
    pub async fn submit_graph(&self) -> ErgataiResult<Vec<String>> {
        // Persist dag_id and started_at on first submission
        {
            let mut graph = self.graph.lock().await;
            if graph.dag_id.is_none() {
                graph.dag_id = Some(self.dag_id.clone());
            }
            if graph.started_at.is_none() {
                graph.started_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }

        // Check if DAG-level deadline has passed
        if let Some(deadline) = self.deadline {
            if std::time::Instant::now() >= deadline {
                tracing::error!(dag_id = %self.dag_id, "DAG deadline already passed before submission");
                return Err(ErgataiError::internal("DAG deadline already passed"));
            }
        }

        // Spawn DAG-level timeout watcher (idempotent — only spawns once)
        self.spawn_dag_timeout_watcher().await;

        // Clear completed/failed agents from previous DAG runs (M14 fix)
        let launcher = super::agent_launcher::AgentLauncher::new(self.project_root.clone());
        launcher.clear_stale_agents().await?;

        // Calculate critical path for priority optimization
        let critical_path_result = self.calculate_critical_path().await;

        // Atomically collect and preempt ready nodes in a single lock acquisition
        // to prevent TOCTOU race condition where concurrent submit_graph calls
        // could submit the same node twice.
        let ready_nodes: Vec<(TaskNode, u32)> = {
            let mut graph = self.graph.lock().await;
            let ready: Vec<TaskNode> = graph
                .ready_tasks()
                .into_iter()
                .filter(|n| n.status == TaskStatus::Pending)
                .cloned()
                .collect();

            // Check conditions and skip nodes that don't meet their conditions
            let mut filtered_ready = Vec::new();
            for node in ready {
                if let Some(ref condition_expr) = node.condition {
                    let context = self.context.lock().await;
                    let condition = ergatai_dag::Condition::new(condition_expr);
                    if !condition.evaluate(&context) {
                        tracing::info!(
                            node_id = %node.id,
                            condition = %condition_expr,
                            "Node condition not met, marking as Skipped"
                        );
                        graph.update_status(&node.id, TaskStatus::Skipped)?;
                        // Also skip downstream nodes that depend on this one
                        Self::skip_downstream_nodes(&mut graph, &node.id)?;
                        continue;
                    }
                }

                // Calculate adjusted priority using CPM
                let base_priority =
                    ergatai_lock::conflict_arbitration::priority_to_number(&node.priority)
                        .map(|p| p as u32)
                        .unwrap_or(2);

                let adjusted_priority = if let Some(ref cpm_result) = critical_path_result {
                    ergatai_dag::critical_path::adjust_priority_with_critical_path(
                        &node,
                        cpm_result,
                        base_priority,
                    )
                } else {
                    base_priority
                };

                filtered_ready.push((node, adjusted_priority));
            }

            // Immediately preempt as Running to prevent duplicate submission
            for (n, _) in &filtered_ready {
                graph.update_status(&n.id, TaskStatus::Running)?;
            }
            filtered_ready
        };

        // Mark progress at the top of submit_graph (once, not inside the per-node loop).
        // A stall watchdog will later compare this timestamp against now.
        self.touch_progress().await;

        let mut submitted = Vec::with_capacity(ready_nodes.len());
        for (node, priority) in ready_nodes {
            match self.generate_and_submit(&node, priority).await {
                Ok(task_id) => {
                    tracing::info!(
                        "Submitted node {} as task {} (priority: {})",
                        node.id,
                        task_id,
                        priority
                    );
                    submitted.push(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to submit node {}: {}", node.id, e);
                    // Revert status so the node can be retried
                    let mut graph = self.graph.lock().await;
                    if let Err(revert_err) = graph.update_status(&node.id, TaskStatus::Pending) {
                        tracing::warn!(
                            "Failed to revert node {} status to Pending after submission error: {}. \
                             Node may be stuck in incorrect state.",
                            node.id, revert_err
                        );
                    }
                }
            }
        }

        // Save graph state — serialize under lock, write without
        self.save_graph_unlocked().await?;

        Ok(submitted)
    }

    /// Calculate critical path for the DAG
    ///
    /// Uses estimated durations from node metadata or defaults to 10 seconds per node.
    /// Returns None if the graph is empty or has no valid start nodes.
    async fn calculate_critical_path(
        &self,
    ) -> Option<ergatai_dag::critical_path::CriticalPathResult> {
        let graph = self.graph.lock().await;

        // Build estimated durations map
        // Try to use node timeout as estimate, otherwise default to 10 seconds
        let mut estimated_durations = std::collections::HashMap::new();
        for node in &graph.nodes {
            let duration = node.timeout.unwrap_or(10);
            estimated_durations.insert(node.id.clone(), duration);
        }

        drop(graph);

        let graph = self.graph.lock().await;
        ergatai_dag::critical_path::calculate_critical_path(&graph, &estimated_durations)
    }

    /// Generate plan and submit to scheduler (no lock acquisition)
    ///
    /// Prefers NATS event publishing when available (decoupled, event-driven).
    /// Falls back to direct `task_scheduler.submit_task()` call otherwise.
    async fn generate_and_submit(&self, node: &TaskNode, priority: u32) -> ErgataiResult<String> {
        // Deadline check: refuse dispatch when the DAG-level timeout has elapsed.
        // Mirrors the budget-check pattern — finalize defensively so the DAG can
        // settle, then propagate the error to the caller.
        if let Some(reason) = self.check_deadline() {
            self.finalize_if_terminal().await;
            return Err(ErgataiError::internal(reason));
        }
        // Budget check: refuse dispatch when DAG-level agent call cap is exhausted.
        // On exhaustion, defensively nudge terminal finalization so the DAG can
        // settle (callers already handle the propagated Err by reverting the
        // node to Pending and logging).
        if let Err(e) = self.check_budget() {
            self.finalize_if_terminal().await;
            return Err(e);
        }
        let new_count = self.increment_agent_calls();
        tracing::debug!(
            dag_id = %self.dag_id,
            count = new_count,
            "agent call dispatched"
        );

        // Generate plan file (still needed — agents read it as a document)
        let plan_file = self.generate_node_plan(node).await?;
        let task_id = node.id.clone();

        if ergatai_nats::is_nats_initialized().await {
            // NATS path: publish task submission event with inline plan content
            if let Some(conn) = ergatai_nats::get_nats_connection().await {
                let bus = ergatai_nats::EventBus::new(conn);
                let plan_content = tokio::fs::read_to_string(&plan_file).await?;
                let dag_id = self.dag_id().to_string();

                let payload = ergatai_nats::TaskSubmitPayload {
                    task_id: task_id.clone(),
                    plan_content,
                    plan_file: plan_file.to_string_lossy().to_string(),
                    target_agent: node.agent.clone(),
                    priority,
                    timeout_secs: node.timeout,
                    dag_id: Some(dag_id),
                };

                bus.publish_task_submit(&payload).await?;
                tracing::info!(task_id = task_id, "Submitted node via NATS event");

                // Start timeout watchdog if timeout is configured
                if let Some(timeout_secs) = node.timeout {
                    self.spawn_timeout_watcher(&task_id, timeout_secs, &node.agent);
                }

                return Ok(task_id);
            }
        }

        // Fallback: direct task_scheduler call
        let tid = self
            .scheduler
            .submit_task_with_priority(plan_file, priority)
            .await?;

        // Start timeout watchdog if timeout is configured
        if let Some(timeout_secs) = node.timeout {
            self.spawn_timeout_watcher(&tid, timeout_secs, &node.agent);
        }

        Ok(tid)
    }

    /// Get the unique DAG identifier (UUID)
    pub fn dag_id(&self) -> &str {
        &self.dag_id
    }

    /// Get elapsed time since DAG creation (for duration reporting)
    fn elapsed_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }

    /// Refresh the last-progress timestamp to `Instant::now()`.
    ///
    /// Called on every observable progress event: a new submit, a node
    /// completion, or a node failure. The stall watchdog (Phase 2) polls
    /// `last_progress_age_secs()` and raises an alarm if this goes stale.
    async fn touch_progress(&self) {
        let mut lp = self.last_progress.lock().await;
        *lp = std::time::Instant::now();
    }

    /// Seconds elapsed since the last progress event.
    ///
    /// A large value indicates the DAG is stalled (no node completions,
    /// failures, or new submits for a while).
    pub async fn last_progress_age_secs(&self) -> u64 {
        let lp = self.last_progress.lock().await;
        lp.elapsed().as_secs()
    }

    /// Returns Ok(()) if budget allows another agent call, or Err if exhausted.
    fn check_budget(&self) -> Result<(), ErgataiError> {
        let Some(limit) = self.max_agent_calls else {
            return Ok(());
        };
        let current = self.agent_call_count.load(Ordering::SeqCst);
        if current >= limit {
            Err(ErgataiError::internal(format!(
                "DAG {} budget exhausted: {} / {} agent calls",
                self.dag_id, current, limit
            )))
        } else {
            Ok(())
        }
    }

    /// Returns Some(reason) if the DAG deadline has passed, None otherwise.
    fn check_deadline(&self) -> Option<String> {
        let deadline = self.deadline?;
        let now = std::time::Instant::now();
        if now >= deadline {
            Some(format!(
                "DAG {} exceeded deadline by {}s",
                self.dag_id,
                now.duration_since(deadline).as_secs()
            ))
        } else {
            None
        }
    }

    fn increment_agent_calls(&self) -> u64 {
        self.agent_call_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Spawn a timeout watchdog for a node.
    ///
    /// If the node has a `timeout` (in seconds), starts a background task that
    /// will mark the node as failed after the timeout elapses. The watchdog is
    /// automatically cancelled when the node completes or fails normally.
    fn spawn_timeout_watcher(&self, node_id: &str, timeout_secs: u64, agent_name: &str) {
        if timeout_secs == 0 {
            return;
        }

        let node_id_owned = node_id.to_string();
        let node_id_for_store = node_id.to_string();
        let agent_name = agent_name.to_string();
        let scheduler = self.clone();
        let watchers = self.timeout_watchers.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)).await;

            // Timeout elapsed — check if node is still running
            let graph = scheduler.graph.lock().await;
            if let Some(node) = graph.find_node(&node_id_owned) {
                if node.status == TaskStatus::Running {
                    tracing::warn!(
                        node_id = %node_id_owned,
                        timeout_secs = timeout_secs,
                        agent = %agent_name,
                        "Node timed out, marking as failed"
                    );
                    drop(graph);

                    // Remove from watcher map before triggering failure
                    {
                        let mut w = watchers.lock().await;
                        w.remove(&node_id_owned);
                    }

                    // Trigger failure handling
                    if let Err(e) = scheduler
                        .on_node_failed(
                            &node_id_owned,
                            &format!("Task timed out after {} seconds", timeout_secs),
                        )
                        .await
                    {
                        tracing::error!(
                            node_id = %node_id_owned,
                            error = %e,
                            "Failed to handle timeout for node"
                        );
                    }
                }
            }
        });

        // Store the handle in the watchers map (via detached spawn to avoid blocking)
        let watchers_store = self.timeout_watchers.clone();
        tokio::spawn(async move {
            let mut w = watchers_store.lock().await;
            w.insert(node_id_for_store, handle);
        });
    }

    /// Cancel the timeout watchdog for a node (called on normal completion/failure)
    async fn cancel_timeout_watcher(&self, node_id: &str) {
        let mut watchers = self.timeout_watchers.lock().await;
        if let Some(handle) = watchers.remove(node_id) {
            handle.abort();
            tracing::debug!(node_id = node_id, "Cancelled timeout watchdog");
        }
    }

    /// Spawn DAG-level timeout watcher (idempotent — only spawns once).
    ///
    /// If the DAG has a `timeout` field, starts a background task that will
    /// fail all remaining Pending/Running nodes when the deadline is reached.
    async fn spawn_dag_timeout_watcher(&self) {
        // Check if already spawned
        {
            let watcher = self.dag_timeout_watcher.lock().await;
            if watcher.is_some() {
                return;
            }
        }

        // Get timeout from graph
        let timeout_secs = {
            let graph = self.graph.lock().await;
            graph.timeout
        };

        let timeout_secs = match timeout_secs {
            Some(t) if t > 0 => t,
            _ => return, // No timeout configured
        };

        let scheduler = self.clone();
        let dag_id = self.dag_id.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)).await;

            tracing::warn!(
                dag_id = %dag_id,
                timeout_secs = timeout_secs,
                "DAG-level timeout reached, failing all remaining nodes"
            );

            if let Err(e) = scheduler
                .fail_all_remaining_nodes("DAG-level timeout reached")
                .await
            {
                tracing::error!(dag_id = %dag_id, error = %e, "Failed to handle DAG timeout");
            }
        });

        let mut watcher = self.dag_timeout_watcher.lock().await;
        *watcher = Some(handle);
    }

    /// Fail all Pending and Running nodes in the DAG (called on DAG timeout or cancellation).
    ///
    /// Marks all non-completed nodes as Failed with the given reason, and publishes
    /// failure events for any Running nodes (so their concurrency permits are released).
    pub async fn fail_all_remaining_nodes(&self, reason: &str) -> ErgataiResult<()> {
        let nodes_to_fail: Vec<(String, String, TaskStatus)> = {
            let graph = self.graph.lock().await;
            graph
                .nodes
                .iter()
                .filter(|n| n.status == TaskStatus::Pending || n.status == TaskStatus::Running)
                .map(|n| (n.id.clone(), n.agent.clone(), n.status.clone()))
                .collect()
        };

        for (node_id, agent, status) in nodes_to_fail {
            tracing::info!(
                node_id = %node_id,
                agent = %agent,
                previous_status = ?status,
                reason = reason,
                "Failing node due to DAG-level constraint"
            );

            // Cancel any node-level timeout watcher
            self.cancel_timeout_watcher(&node_id).await;

            // Cancel running task if it was Running (releases concurrency permit)
            if status == TaskStatus::Running {
                self.scheduler.cancel_running_task(&node_id).await;
            }

            // Mark as failed
            if let Err(e) = self.on_node_failed(&node_id, reason).await {
                tracing::warn!(
                    node_id = %node_id,
                    error = %e,
                    "Failed to handle node failure during DAG timeout"
                );
            }
        }

        Ok(())
    }

    /// Start listening for DAG events via JetStream pull consumer
    ///
    /// Pulls messages from the `DAG_EVENTS` stream with filter `ergatai.dag.>`.
    /// Dispatches by subject:
    /// - `ergatai.dag.node_complete.*` → `on_node_completed()`
    /// - `ergatai.dag.node_failed.*`   → `on_node_failed()`
    /// - `ergatai.dag.complete.*`      → logged (no handler yet)
    ///
    /// Returns a `JoinHandle` that can be aborted to stop listening.
    pub fn start_event_listener(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let conn = match ergatai_nats::get_nats_connection().await {
                Some(c) => c,
                None => {
                    tracing::warn!("NATS not initialized, event listener not started");
                    return;
                }
            };

            let mut messages = match init_dag_event_consumer(&conn).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to initialize DAG event consumer");
                    return;
                }
            };

            tracing::info!(
                "JetStream DAG event listener started (stream: {}, filter: ergatai.dag.>)",
                ergatai_nats::DAG_EVENTS_STREAM
            );

            use futures_util::StreamExt;
            loop {
                let next =
                    tokio::time::timeout(std::time::Duration::from_secs(5), messages.next()).await;

                match next {
                    Err(_) => continue, // idle timeout — loop for abort check

                    Ok(None) => {
                        tracing::warn!("DAG event stream closed, listener exiting");
                        break;
                    }

                    Ok(Some(Ok(js_msg))) => {
                        handle_dag_event(&js_msg, &self).await;
                    }

                    Ok(Some(Err(e))) => {
                        tracing::warn!(error = %e, "Error receiving DAG event from stream");
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

        // Pre-allocate for header + upstream dependencies
        let mut lines = Vec::with_capacity(2 + node.depends_on.len());
        lines.push(String::new());
        lines.push("### Upstream Context".to_string());

        for dep_id in &node.depends_on {
            // Find the dependency node to get its human-readable name
            let dep_name = graph
                .find_node(dep_id)
                .map(|n| n.task.as_str())
                .unwrap_or(dep_id);

            if let Some(outputs) = ctx.get_node_outputs(dep_id) {
                // Check if the JSON value is a non-empty object
                let is_non_empty_object =
                    matches!(outputs, serde_json::Value::Object(obj) if !obj.is_empty());

                if is_non_empty_object {
                    lines.push(format!("\n**{}** ({}) outputs:", dep_name, dep_id));
                    if let serde_json::Value::Object(obj) = outputs {
                        for (k, v) in obj {
                            lines.push(format!("  - {}: {}", k, v));
                        }
                    }
                } else {
                    lines.push(format!(
                        "\n**{}** ({}) — completed (no outputs recorded)",
                        dep_name, dep_id
                    ));
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
        // Deadline check: short-circuit if the DAG has exceeded its timeout.
        if let Some(reason) = self.check_deadline() {
            self.finalize_if_terminal().await;
            return Err(ErgataiError::internal(reason));
        }
        // Node completion is observable progress — refresh the stall watchdog timestamp.
        self.touch_progress().await;
        // Cancel timeout watchdog (node completed normally)
        self.cancel_timeout_watcher(node_id).await;

        // Calculate critical path for priority optimization
        let critical_path_result = self.calculate_critical_path().await;

        // Update completed node status AND atomically preempt ready nodes as Running
        // within a single lock acquisition to prevent TOCTOU duplicate submission.
        let ready_nodes: Vec<(TaskNode, u32)> = {
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

            let mut ready_with_priority = Vec::new();
            for node in ready {
                // Calculate adjusted priority using CPM
                let base_priority =
                    ergatai_lock::conflict_arbitration::priority_to_number(&node.priority)
                        .map(|p| p as u32)
                        .unwrap_or(2);

                let adjusted_priority = if let Some(ref cpm_result) = critical_path_result {
                    ergatai_dag::critical_path::adjust_priority_with_critical_path(
                        &node,
                        cpm_result,
                        base_priority,
                    )
                } else {
                    base_priority
                };

                ready_with_priority.push((node, adjusted_priority));
            }

            for (n, _) in &ready_with_priority {
                graph.update_status(&n.id, TaskStatus::Running)?;
            }
            ready_with_priority
        };

        tracing::info!(
            "Node {} completed, {} newly ready nodes preempted",
            node_id,
            ready_nodes.len()
        );

        let mut newly_submitted = Vec::with_capacity(ready_nodes.len());
        for (node, priority) in ready_nodes {
            match self.generate_and_submit(&node, priority).await {
                Ok(task_id) => {
                    tracing::info!(
                        "Submitted newly ready node {} as task {} (priority: {})",
                        node.id,
                        task_id,
                        priority
                    );
                    newly_submitted.push(task_id);
                }
                Err(e) => {
                    tracing::error!("Failed to submit node {}: {}", node.id, e);
                    // Revert status so the node can be retried
                    let mut graph = self.graph.lock().await;
                    if let Err(revert_err) = graph.update_status(&node.id, TaskStatus::Pending) {
                        tracing::warn!(
                            "Failed to revert node {} status to Pending after submission error: {}. \
                             Node may be stuck in incorrect state.",
                            node.id, revert_err
                        );
                    }
                }
            }
        }

        // Save graph + context together
        self.save_graph_unlocked().await?;

        // Check if all done
        self.finalize_if_terminal().await;

        Ok(newly_submitted)
    }

    /// Called when a node fails.
    ///
    /// Concurrency note: NATS at-least-once delivery may dispatch duplicate
    /// `node_failed` events concurrently. All state inspection and transitions
    /// happen inside a single lock acquisition so that only the first caller
    /// "claims" the node - subsequent concurrent calls see a non-Running
    /// status and return early without consuming retry budget or
    /// double-submitting the task.
    pub async fn on_node_failed(&self, node_id: &str, error: &str) -> ErgataiResult<()> {
        // Deadline check: short-circuit if the DAG has exceeded its timeout.
        if let Some(reason) = self.check_deadline() {
            self.finalize_if_terminal().await;
            return Err(ErgataiError::internal(reason));
        }
        // Node failure is observable progress — refresh the stall watchdog timestamp.
        self.touch_progress().await;
        // Cancel timeout watchdog (node already failed)
        self.cancel_timeout_watcher(node_id).await;

        // All state inspection + transition under one lock hold.
        // `retry_decision` is `Some((node_clone, retry_count))` when we should retry,
        // or `None` when retries are exhausted (node marked Failed inside the lock).
        let retry_decision: Option<(TaskNode, u32)> = {
            let mut graph = self.graph.lock().await;
            let node = graph.find_node_mut(node_id).ok_or_else(|| {
                ErgataiError::InvalidArgument(format!("Node not found: {}", node_id))
            })?;

            // Guard: only process if the node is still in Running state.
            // - Normal entry: node was Running when the failure was reported.
            // - Concurrent handler already claimed it: status is Pending (retry
            //   in flight) or Failed (terminal / already processed).
            // This is the atomic "claim" that prevents duplicate retries.
            if node.status != TaskStatus::Running {
                tracing::debug!(
                    node_id = node_id,
                    status = ?node.status,
                    "Node not in Running state, skipping retry handling"
                );
                return Ok(());
            }

            if node.retry_count < node.max_retries {
                // Atomically: bump retry count + move to Pending.
                // Pending both records "will retry" and prevents a concurrent
                // handler from re-claiming the node.
                node.retry_count += 1;
                node.status = TaskStatus::Pending;
                let retry_count = node.retry_count;
                Some((node.clone(), retry_count))
            } else {
                // Retries exhausted - mark terminal.
                node.status = TaskStatus::Failed;
                None
            }
        }; // Lock released

        if let Some((node_clone, retry_count)) = retry_decision {
            // Calculate critical path for priority optimization
            let critical_path_result = self.calculate_critical_path().await;

            // Exponential backoff with jitter: base * 2^(retry_count-1) + random(0, base)
            let base_delay = 3u64; // seconds
            let exponential = base_delay * (1u64 << (retry_count - 1).min(6)); // cap at 2^6 = 64
            let jitter = rand_delay(base_delay);
            let delay = std::time::Duration::from_secs(exponential + jitter);

            tracing::info!(
                "Node {} failed, retrying in {:?} (attempt {}, backoff {}s + jitter {}s)",
                node_id,
                delay,
                retry_count,
                exponential,
                jitter,
            );

            // Wait before retrying (no locks held)
            tokio::time::sleep(delay).await;

            // Calculate priority for retry
            let base_priority =
                ergatai_lock::conflict_arbitration::priority_to_number(&node_clone.priority)
                    .map(|p| p as u32)
                    .unwrap_or(2);

            let priority = if let Some(ref cpm_result) = critical_path_result {
                ergatai_dag::critical_path::adjust_priority_with_critical_path(
                    &node_clone,
                    cpm_result,
                    base_priority,
                )
            } else {
                base_priority
            };

            // Submit without holding lock
            match self.generate_and_submit(&node_clone, priority).await {
                Ok(_task_id) => {
                    let mut graph = self.graph.lock().await;
                    if let Err(e) = graph.update_status(node_id, TaskStatus::Running) {
                        tracing::warn!(
                            "Failed to update node {} status to Running after successful retry: {}",
                            node_id,
                            e
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to retry node {}: {}", node_id, e);
                    let mut graph = self.graph.lock().await;
                    if let Err(status_err) = graph.update_status(node_id, TaskStatus::Failed) {
                        tracing::warn!(
                            "Failed to update node {} status to Failed: {}",
                            node_id,
                            status_err
                        );
                    }
                    drop(graph);
                    self.save_graph_unlocked().await?;
                }
            }
        } else {
            tracing::error!("Node {} failed: {} (no more retries)", node_id, error);
            {
                let mut graph = self.graph.lock().await;
                // Status already set to Failed inside the claim lock above,
                // but enforce it defensively in case of future edits.
                if let Err(e) = graph.update_status(node_id, TaskStatus::Failed) {
                    tracing::warn!(
                        "Failed to defensively set node {} status to Failed: {}",
                        node_id,
                        e
                    );
                }
            }

            // Propagate failure: skip all downstream nodes
            self.skip_downstream(node_id).await?;

            self.save_graph_unlocked().await?;

            // If cascading failures left the DAG fully terminal, finalize.
            self.finalize_if_terminal().await;
        }

        Ok(())
    }

    /// If the DAG has reached a terminal state (all nodes are either
    /// `Completed`, `Failed`, or `Skipped`), publish a `DagCompletePayload`
    /// event via NATS and remove this scheduler from the global registry so
    /// the `CollaborationSession` (MeshPolicy ACL) stops applying to
    /// `send_message` calls. Agents then regain unrestricted communication.
    ///
    /// Idempotent: an internal `AtomicBool` gate ensures only the first
    /// concurrent caller proceeds — subsequent callers return early even if
    /// they also observe `is_complete() == true`.
    async fn finalize_if_terminal(&self) {
        // Atomic CAS: only one concurrent caller proceeds past this point.
        if self.finalized.swap(true, Ordering::SeqCst) {
            return;
        }

        let is_done = {
            let graph = self.graph.lock().await;
            graph.is_complete()
        };
        if !is_done {
            // DAG not actually terminal — release the gate so a later caller
            // can finalize when it truly completes.
            self.finalized.store(false, Ordering::SeqCst);
            return;
        }

        tracing::info!(
            dag_id = %self.dag_id,
            "DAG reached terminal state — finalizing"
        );

        // Publish DAG completion event via NATS (best-effort).
        if ergatai_nats::is_nats_initialized().await {
            if let Some(conn) = ergatai_nats::get_nats_connection().await {
                let bus = ergatai_nats::EventBus::new(conn);
                let graph = self.graph.lock().await;
                let total = graph.nodes.len() as u32;
                let (completed, failed) =
                    graph
                        .nodes
                        .iter()
                        .fold((0u32, 0u32), |(c, f), n| match n.status {
                            TaskStatus::Completed => (c + 1, f),
                            TaskStatus::Failed => (c, f + 1),
                            _ => (c, f),
                        });
                drop(graph);

                let payload = ergatai_nats::DagCompletePayload {
                    dag_id: self.dag_id().to_string(),
                    total_nodes: total,
                    completed_nodes: completed,
                    failed_nodes: failed,
                    duration_secs: self.elapsed_secs(),
                };
                if let Err(e) = bus.publish_dag_complete(&payload).await {
                    tracing::error!(error = %e, "Failed to publish DAG complete event");
                }
            }
        }

        // DAG execution finished: remove the scheduler from the global registry
        // so the collaboration session (MeshPolicy ACL) stops applying to
        // send_message calls. Agents regain unrestricted communication.
        clear_dag_scheduler_by_id(Some(&self.dag_id));
        tracing::info!(
            dag_id = %self.dag_id,
            "DAG terminal — collaboration session cleared from registry"
        );
    }

    /// Skip all nodes that (transitively) depend on the failed node.
    async fn skip_downstream(&self, failed_id: &str) -> ErgataiResult<()> {
        // 1. BFS to collect all transitively dependent pending nodes.
        let to_skip: Vec<String> = {
            let graph = self.graph.lock().await;
            let mut queue = vec![failed_id.to_string()];
            let mut to_skip = Vec::with_capacity(graph.nodes.len() / 2);
            let mut seen = std::collections::HashSet::new(); // O(1) lookup

            while let Some(current) = queue.pop() {
                for node in &graph.nodes {
                    if node.depends_on.contains(&current)
                        && node.status == TaskStatus::Pending
                        && seen.insert(&node.id)
                    // O(1) check + insert
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

    /// Skip all nodes that (transitively) depend on the skipped/failed node.
    /// Static helper that works on a mutable graph reference (no self required).
    fn skip_downstream_nodes(graph: &mut TaskGraph, failed_id: &str) -> ErgataiResult<()> {
        // BFS to collect all transitively dependent pending nodes
        let mut queue = vec![failed_id.to_string()];
        let mut seen = std::collections::HashSet::new();

        while let Some(current) = queue.pop() {
            for node in &graph.nodes {
                if node.depends_on.contains(&current)
                    && node.status == TaskStatus::Pending
                    && seen.insert(node.id.clone())
                {
                    queue.push(node.id.clone());
                }
            }
        }

        // Batch-update all skipped nodes
        for node_id in &seen {
            if let Some(node) = graph.find_node_mut(node_id) {
                node.status = TaskStatus::Skipped;
                tracing::info!(
                    "Skipped node {} (depends on skipped/failed {})",
                    node_id,
                    failed_id
                );
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
        // Use per-DAG filenames to support multiple concurrent DAGs
        let dag_id_safe = self
            .dag_id
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");

        // Serialize graph
        let graph_json = {
            let graph = self.graph.lock().await;
            serde_json::to_string(&*graph)
                .map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))?
        };
        let graph_file = ergatai_dir.join(format!("dag-state-{}.json", dag_id_safe));
        tokio::fs::write(&graph_file, graph_json.as_bytes()).await?;

        // Serialize context
        let context_json = {
            let ctx = self.context.lock().await;
            serde_json::to_string(&*ctx)
                .map_err(|e| ErgataiError::json_with_source("Failed to serialize context", e))?
        };
        let context_file = ergatai_dir.join(format!("dag-context-{}.json", dag_id_safe));
        tokio::fs::write(&context_file, context_json.as_bytes()).await?;

        Ok(())
    }

    /// Load graph and context from disk (for recovery)
    pub async fn load_from_disk(project_root: PathBuf) -> ErgataiResult<Self> {
        // Try the legacy single-DAG filename first (backward compatibility)
        let legacy_graph_file = project_root.join(".ergatai").join("dag-state.json");
        if legacy_graph_file.exists() {
            let graph = TaskGraph::load_from_file(&legacy_graph_file).await?;
            let context_file = project_root.join(".ergatai").join("dag-context.json");
            let context = if context_file.exists() {
                DagContext::load_from_file(&context_file).await?
            } else {
                DagContext::empty()
            };
            return Ok(Self::with_context(project_root, graph, context));
        }

        // Load the most recent DAG (by modification time) from per-DAG files
        let ergatai_dir = project_root.join(".ergatai");
        let mut dag_files: Vec<PathBuf> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&ergatai_dir).await {
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext.to_str() == Some("json") {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if name.starts_with("dag-state-") {
                                dag_files.push(path);
                            }
                        }
                    }
                }
            }
        }

        if dag_files.is_empty() {
            return Err(ErgataiError::NotFound(
                "No DAG state files found".to_string(),
            ));
        }

        // Sort by modification time (most recent first)
        dag_files.sort_by(|a, b| {
            let a_time = a.metadata().and_then(|m| m.modified()).ok();
            let b_time = b.metadata().and_then(|m| m.modified()).ok();
            b_time.cmp(&a_time)
        });

        let graph_file = &dag_files[0];
        let graph = TaskGraph::load_from_file(graph_file).await?;

        // Derive context filename from graph filename
        let context_file = graph_file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("dag-state-"))
            .map(|id| ergatai_dir.join(format!("dag-context-{}", id)))
            .ok_or_else(|| ErgataiError::NotFound("Invalid DAG state filename".to_string()))?;

        let context = if context_file.exists() {
            DagContext::load_from_file(&context_file).await?
        } else {
            DagContext::empty()
        };

        Ok(Self::with_context(project_root, graph, context))
    }

    /// Load all DAGs from disk (for multi-DAG recovery)
    pub async fn load_all_from_disk(project_root: PathBuf) -> ErgataiResult<Vec<Self>> {
        let ergatai_dir = project_root.join(".ergatai");

        // Collect all DAG state files
        let mut dag_files: Vec<PathBuf> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&ergatai_dir).await {
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext.to_str() == Some("json") {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if name.starts_with("dag-state-") {
                                dag_files.push(path);
                            }
                        }
                    }
                }
            }
        }

        let mut schedulers = Vec::new();
        for graph_file in dag_files {
            match TaskGraph::load_from_file(&graph_file).await {
                Ok(graph) => {
                    // Derive context filename
                    let context_file = graph_file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_prefix("dag-state-"))
                        .map(|id| ergatai_dir.join(format!("dag-context-{}", id)));

                    let context = if let Some(ref ctx_file) = context_file {
                        if ctx_file.exists() {
                            DagContext::load_from_file(ctx_file).await.unwrap_or_else(|e| {
                                tracing::warn!(file = ?ctx_file, error = %e, "Failed to load context file, using empty context");
                                DagContext::empty()
                            })
                        } else {
                            DagContext::empty()
                        }
                    } else {
                        DagContext::empty()
                    };

                    schedulers.push(Self::with_context(project_root.clone(), graph, context));
                }
                Err(e) => {
                    tracing::warn!(file = ?graph_file, error = %e, "Failed to load DAG state file");
                }
            }
        }

        // Also try loading legacy single-DAG file
        let legacy_graph_file = project_root.join(".ergatai").join("dag-state.json");
        if legacy_graph_file.exists() {
            if let Ok(graph) = TaskGraph::load_from_file(&legacy_graph_file).await {
                let context_file = project_root.join(".ergatai").join("dag-context.json");
                let context = if context_file.exists() {
                    DagContext::load_from_file(&context_file).await.unwrap_or_else(|e| {
                        tracing::warn!(file = ?context_file, error = %e, "Failed to load legacy context file, using empty context");
                        DagContext::empty()
                    })
                } else {
                    DagContext::empty()
                };
                schedulers.push(Self::with_context(project_root.clone(), graph, context));
            }
        }

        Ok(schedulers)
    }

    /// Rollback all Running nodes to Pending (for recovery after crash)
    ///
    /// When the server crashes, nodes left in Running state are actually stopped.
    /// This method resets them to Pending so they can be resubmitted on recovery.
    pub async fn rollback_running_nodes(&self) -> ErgataiResult<()> {
        let mut graph = self.graph.lock().await;
        let mut rolled_back = 0;
        for node in &mut graph.nodes {
            if node.status == TaskStatus::Running {
                node.status = TaskStatus::Pending;
                node.retry_count = 0; // Reset retry count for fresh start
                rolled_back += 1;
            }
        }
        drop(graph);

        if rolled_back > 0 {
            tracing::info!(
                dag_id = %self.dag_id,
                rolled_back,
                "Rolled back Running nodes to Pending for recovery"
            );
            self.save_graph_unlocked().await?;
        }

        Ok(())
    }

    /// Get a JSON snapshot of the current graph state
    pub async fn graph_snapshot(&self) -> ErgataiResult<String> {
        let graph = self.graph.lock().await;
        serde_json::to_string(&*graph)
            .map_err(|e| ErgataiError::json_with_source("Failed to serialize graph", e))
    }
}

/// Generate a random delay value in [0, max_secs) for jitter.
///
/// Uses a simple approach without external rand crate: hash the current
/// time with a counter to get pseudo-random bits.
fn rand_delay(max_secs: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    // Add thread id for extra entropy across concurrent retries
    std::thread::current().id().hash(&mut hasher);
    hasher.finish() % max_secs
}

/// Initialize the JetStream pull consumer for DAG events on the DAG_EVENTS stream.
///
/// Returns a boxed stream of JetStream messages filtered to `ergatai.dag.>`.
/// The consumer is durable (`dag_events`) and resumes from the last ack on restart.
async fn init_dag_event_consumer(
    connection: &ergatai_nats::NatsConnection,
) -> ErgataiResult<
    futures_util::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
> {
    ergatai_nats::init_dag_stream_pull_consumer(
        connection,
        ergatai_nats::DAG_EVENTS_CONSUMER,
        "ergatai.dag.>",
    )
    .await
    .map_err(ErgataiError::NatsError)
}

/// Handle a single DAG event by subject-prefix dispatch.
///
/// - `ergatai.dag.node_complete.*` → deserialize NodeCompletePayload, run `on_node_completed`
/// - `ergatai.dag.node_failed.*`   → deserialize NodeFailedPayload,  run `on_node_failed`
/// - `ergatai.dag.complete.*`      → deserialize DagCompletePayload, log (no action yet)
///
/// Acks on success; naks on handler error; acks malformed messages to discard.
async fn handle_dag_event(js_msg: &async_nats::jetstream::Message, scheduler: &DagScheduler) {
    let subject = js_msg.subject.as_str();

    // ── node_complete.* ──
    if subject.starts_with("ergatai.dag.node_complete.") {
        let payload: ergatai_nats::NodeCompletePayload = match serde_json::from_slice(
            &js_msg.payload,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, subject = subject, "Malformed node_complete — acking to discard");
                let _ = js_msg.ack().await;
                return;
            }
        };

        tracing::info!(node_id = %payload.node_id, "Received JetStream node_complete event");

        // Check if outputs is a non-empty object
        let has_outputs =
            matches!(&payload.outputs, serde_json::Value::Object(obj) if !obj.is_empty());
        if has_outputs {
            scheduler
                .record_outputs(&payload.node_id, payload.outputs)
                .await;
        }

        match scheduler
            .on_node_completed(&payload.node_id, payload.result_file)
            .await
        {
            Ok(newly_submitted) => {
                tracing::info!(
                    node_id = %payload.node_id,
                    newly_submitted = newly_submitted.len(),
                    "Processed node_complete, submitted downstream"
                );
                let _ = js_msg.ack().await;
            }
            Err(e) => {
                tracing::error!(node_id = %payload.node_id, error = %e, "Failed to process node_complete — naking");
                let _ = js_msg
                    .ack_with(async_nats::jetstream::message::AckKind::Nak(None))
                    .await;
            }
        }
        return;
    }

    // ── node_failed.* ──
    if subject.starts_with("ergatai.dag.node_failed.") {
        let payload: ergatai_nats::NodeFailedPayload = match serde_json::from_slice(&js_msg.payload)
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, subject = subject, "Malformed node_failed — acking to discard");
                let _ = js_msg.ack().await;
                return;
            }
        };

        tracing::info!(node_id = %payload.node_id, error = %payload.error, "Received JetStream node_failed event");

        match scheduler
            .on_node_failed(&payload.node_id, &payload.error)
            .await
        {
            Ok(()) => {
                tracing::info!(node_id = %payload.node_id, "Processed node_failed");
                let _ = js_msg.ack().await;
            }
            Err(e) => {
                tracing::error!(node_id = %payload.node_id, error = %e, "Failed to process node_failed — naking");
                let _ = js_msg
                    .ack_with(async_nats::jetstream::message::AckKind::Nak(None))
                    .await;
            }
        }
        return;
    }

    // ── complete.* (informational) ──
    if subject.starts_with("ergatai.dag.complete.") {
        match serde_json::from_slice::<ergatai_nats::DagCompletePayload>(&js_msg.payload) {
            Ok(payload) => {
                tracing::info!(
                    dag_id = %payload.dag_id,
                    completed = payload.completed_nodes,
                    failed = payload.failed_nodes,
                    total = payload.total_nodes,
                    "Received JetStream dag_complete event"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, subject = subject, "Malformed dag_complete — acking to discard");
            }
        }
        let _ = js_msg.ack().await;
        return;
    }

    // ── Unknown subject under ergatai.dag.> — ack to discard ──
    tracing::warn!(
        subject = subject,
        "Unhandled DAG event subject — acking to discard"
    );
    let _ = js_msg.ack().await;
}

// ── Global DAG Scheduler Registry ──

use std::sync::Mutex as StdMutex;

static GLOBAL_DAGS: std::sync::OnceLock<StdMutex<HashMap<String, DagScheduler>>> =
    std::sync::OnceLock::new();

fn dag_registry() -> &'static StdMutex<HashMap<String, DagScheduler>> {
    GLOBAL_DAGS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Set the active DAG scheduler (replaces any existing one with the same dag_id)
pub fn set_dag_scheduler(scheduler: DagScheduler) {
    let dag_id = scheduler.dag_id().to_string();
    match dag_registry().lock() {
        Ok(mut guard) => {
            guard.insert(dag_id, scheduler);
        }
        Err(poisoned) => {
            tracing::error!("Global DAG registry lock poisoned, recovering");
            poisoned.into_inner().insert(dag_id, scheduler);
        }
    }
}

/// Get a clone of the active DAG scheduler by dag_id, or the most recent one if dag_id is None
pub fn get_dag_scheduler() -> Option<DagScheduler> {
    get_dag_scheduler_by_id(None)
}

/// Get a clone of a specific DAG scheduler by dag_id
pub fn get_dag_scheduler_by_id(dag_id: Option<&str>) -> Option<DagScheduler> {
    match dag_registry().lock() {
        Ok(guard) => {
            if let Some(id) = dag_id {
                guard.get(id).cloned()
            } else {
                // Return the most recently added DAG (last inserted)
                guard.values().last().cloned()
            }
        }
        Err(poisoned) => {
            tracing::error!("Global DAG registry lock poisoned, recovering");
            let guard = poisoned.into_inner();
            if let Some(id) = dag_id {
                guard.get(id).cloned()
            } else {
                guard.values().last().cloned()
            }
        }
    }
}

/// List all active DAG schedulers
pub fn list_dag_schedulers() -> Vec<DagScheduler> {
    match dag_registry().lock() {
        Ok(guard) => guard.values().cloned().collect(),
        Err(poisoned) => {
            tracing::error!("Global DAG registry lock poisoned, recovering");
            poisoned.into_inner().values().cloned().collect()
        }
    }
}

/// Clear a specific DAG scheduler by dag_id, or all if dag_id is None
pub fn clear_dag_scheduler() {
    clear_dag_scheduler_by_id(None)
}

/// Clear a specific DAG scheduler by dag_id
pub fn clear_dag_scheduler_by_id(dag_id: Option<&str>) {
    match dag_registry().lock() {
        Ok(mut guard) => {
            if let Some(id) = dag_id {
                guard.remove(id);
            } else {
                guard.clear();
            }
        }
        Err(poisoned) => {
            tracing::error!("Global DAG registry lock poisoned, recovering");
            let mut guard = poisoned.into_inner();
            if let Some(id) = dag_id {
                guard.remove(id);
            } else {
                guard.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ergatai_dag::TaskNode;

    /// Global test lock — serializes tests that share the `GLOBAL_DAG` static.
    ///
    /// `test_global_dag_scheduler_lifecycle` and `test_global_dag_scheduler_replace`
    /// both call `clear_dag_scheduler()` / `set_dag_scheduler()` which mutate the
    /// same `OnceLock<Mutex<Option<DagScheduler>>>`. Running them in parallel
    /// causes race conditions. This lock ensures sequential execution.
    static TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    fn sample_graph() -> TaskGraph {
        TaskGraph::new(vec![
            TaskNode::new("n1", "agent-a", "Task A"),
            TaskNode::new("n2", "agent-b", "Task B").with_dependencies(vec!["n1".into()]),
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

        // Mark n1 as Running then failed (no retries configured).
        // on_node_failed requires the node to be in Running state -
        // this mirrors the real flow where an executing task fails.
        {
            let mut graph = scheduler.graph.lock().await;
            graph.update_status("n1", TaskStatus::Running).unwrap();
        }
        scheduler.on_node_failed("n1", "boom").await.unwrap();

        // Check that n1 is Failed and n2, n3 are Skipped
        let graph = scheduler.graph.lock().await;
        assert_eq!(graph.find_node("n1").unwrap().status, TaskStatus::Failed);
        assert_eq!(graph.find_node("n2").unwrap().status, TaskStatus::Skipped);
        assert_eq!(graph.find_node("n3").unwrap().status, TaskStatus::Skipped);
    }

    #[tokio::test]
    async fn test_global_dag_scheduler_lifecycle() {
        let _guard = TEST_LOCK.lock().await;
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
        let _guard = TEST_LOCK.lock().await;
        clear_dag_scheduler();

        // Set first scheduler
        let graph1 = sample_graph();
        set_dag_scheduler(DagScheduler::new(PathBuf::from("/tmp"), graph1));
        assert!(get_dag_scheduler().is_some());

        // Replace with second scheduler
        let graph2 = TaskGraph::new(vec![TaskNode::new("x1", "agent", "Task X")]);
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
            .with_input(
                "Fix issues found in review: {{n1.review_result}}. Query: {{global.user_query}}",
            );

        let graph = TaskGraph::new(vec![TaskNode::new("n1", "agent-a", "Review code"), node_b]);

        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = DagContext::new({
            let mut m = HashMap::new();
            m.insert("user_query".to_string(), "improve performance".to_string());
            m
        });

        let scheduler = DagScheduler::with_context(temp_dir.path().to_path_buf(), graph, ctx);

        // Simulate: node n1 completes with outputs
        let mut outputs = serde_json::Map::new();
        outputs.insert(
            "review_result".to_string(),
            serde_json::Value::String("3 issues found: unused imports".to_string()),
        );
        scheduler
            .record_outputs("n1", serde_json::Value::Object(outputs))
            .await;

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

    #[tokio::test]
    async fn test_dag_id_is_unique() {
        let graph = sample_graph();
        let path = PathBuf::from("/tmp/project-x");
        let s1 = DagScheduler::new(path.clone(), graph.clone());
        let s2 = DagScheduler::new(path, TaskGraph::new(vec![]));
        // Each scheduler gets a unique UUID-based dag_id
        assert_ne!(s1.dag_id(), s2.dag_id());
        assert!(s1.dag_id().starts_with("dag-"));
        // UUID format: dag-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        assert!(s1.dag_id().len() > 10);
    }

    #[tokio::test]
    async fn test_dag_id_differs_for_different_paths() {
        let s1 = DagScheduler::new(PathBuf::from("/tmp/a"), sample_graph());
        let s2 = DagScheduler::new(PathBuf::from("/tmp/b"), sample_graph());
        // Different schedulers always have different IDs (UUID-based)
        assert_ne!(s1.dag_id(), s2.dag_id());
    }

    #[tokio::test]
    async fn test_graph_snapshot_returns_valid_json() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/snap"), graph);
        let snapshot = scheduler.graph_snapshot().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        // Snapshot should contain the graph's nodes array
        assert!(parsed.get("nodes").is_some());
        assert_eq!(parsed["nodes"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_set_global_and_record_outputs_persist_in_context() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/ctx"), graph);

        scheduler.set_global("greeting", "hello").await;
        let mut outputs = serde_json::Map::new();
        outputs.insert(
            "result".to_string(),
            serde_json::Value::String("done".to_string()),
        );
        scheduler
            .record_outputs("n1", serde_json::Value::Object(outputs))
            .await;

        let ctx = scheduler.context();
        let ctx = ctx.lock().await;
        assert_eq!(ctx.get_global("greeting"), Some("hello"));
        let outputs = ctx.get_node_outputs("n1");
        assert!(outputs.is_some());
        if let Some(serde_json::Value::Object(obj)) = outputs {
            assert_eq!(obj.get("result").and_then(|v| v.as_str()), Some("done"));
        } else {
            panic!("Expected Object");
        }
    }

    #[tokio::test]
    async fn test_is_complete_true_when_all_nodes_completed() {
        let graph = TaskGraph::new(vec![TaskNode::new("n1", "a", "A")]);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/complete"), graph);

        // Mark the only node as completed directly on the graph
        {
            let mut g = scheduler.graph.lock().await;
            g.update_status("n1", TaskStatus::Completed).unwrap();
        }
        assert!(scheduler.is_complete().await);
    }

    #[tokio::test]
    async fn test_is_complete_false_with_pending_nodes() {
        let graph = TaskGraph::new(vec![TaskNode::new("n1", "a", "A")]);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/incomplete"), graph);
        assert!(!scheduler.is_complete().await);
    }

    #[tokio::test]
    async fn test_progress_increases_after_completion() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "a", "A"),
            TaskNode::new("n2", "a", "B"),
        ]);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/progress"), graph);
        assert_eq!(scheduler.progress().await, 0.0);

        {
            let mut g = scheduler.graph.lock().await;
            g.update_status("n1", TaskStatus::Completed).unwrap();
        }
        let p = scheduler.progress().await;
        assert!((p - 0.5).abs() < 0.01, "expected ~0.5 progress, got {}", p);
    }

    #[tokio::test]
    async fn test_on_node_completed_with_no_ready_downstream() {
        // Linear chain A → B, only A is initially ready
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "a", "A"),
            TaskNode::new("n2", "a", "B").with_dependencies(vec!["n1".into()]),
        ]);
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);

        // Mark n1 as Running (so it "completes" realistically)
        {
            let mut g = scheduler.graph.lock().await;
            g.update_status("n1", TaskStatus::Running).unwrap();
        }

        // on_node_completed needs to find n1 and mark it complete.
        // However, since submit_graph's ready-task preemption would set n2 to Running,
        // and we don't have a real TaskScheduler backing this, newly_submitted should be empty
        // because generate_and_submit will fail to launch anything.
        // We just verify the node status is updated.
        let _ = scheduler
            .on_node_completed("n1", Some("/tmp/r.md".to_string()))
            .await;

        let g = scheduler.graph.lock().await;
        assert_eq!(g.find_node("n1").unwrap().status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn test_on_node_failed_retry_increments_count() {
        let mut node = TaskNode::new("n1", "a", "A");
        node.max_retries = 3;
        let graph = TaskGraph::new(vec![node]);

        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".ergatai")).unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);

        // Move n1 to Running
        {
            let mut g = scheduler.graph.lock().await;
            g.update_status("n1", TaskStatus::Running).unwrap();
        }

        // First failure: on_node_failed bumps retry_count to 1 and attempts to re-submit.
        // In this test environment generate_and_submit fails (no ergatai_lock init),
        // so the error path sets status back to Failed — but retry_count was already bumped.
        scheduler.on_node_failed("n1", "oops").await.unwrap();
        let g = scheduler.graph.lock().await;
        let n = g.find_node("n1").unwrap();
        assert_eq!(
            n.retry_count, 1,
            "retry_count should have been bumped before submission"
        );
    }

    #[tokio::test]
    async fn test_on_node_failed_exhausted_retries_marks_failed() {
        let mut node = TaskNode::new("n1", "a", "A");
        node.max_retries = 1;
        node.retry_count = 1; // already used up retries
        let graph = TaskGraph::new(vec![node]);

        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".ergatai")).unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);

        {
            let mut g = scheduler.graph.lock().await;
            g.update_status("n1", TaskStatus::Running).unwrap();
        }

        scheduler
            .on_node_failed("n1", "final failure")
            .await
            .unwrap();
        let g = scheduler.graph.lock().await;
        let n = g.find_node("n1").unwrap();
        assert_eq!(n.status, TaskStatus::Failed);
        // retry_count should not have increased (already at max)
        assert_eq!(n.retry_count, 1);
    }

    #[tokio::test]
    async fn test_on_node_failed_ignores_non_running_node() {
        let graph = TaskGraph::new(vec![TaskNode::new("n1", "a", "A")]);
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);

        // n1 is Pending (not Running) - should be a no-op
        scheduler.on_node_failed("n1", "err").await.unwrap();
        let g = scheduler.graph.lock().await;
        // Should remain Pending, not Failed
        assert_eq!(g.find_node("n1").unwrap().status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn test_skip_downstream_transitive() {
        // Chain: n1 → n2 → n3, all pending
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "a", "A"),
            TaskNode::new("n2", "a", "B").with_dependencies(vec!["n1".into()]),
            TaskNode::new("n3", "a", "C").with_dependencies(vec!["n2".into()]),
            // Unrelated node that should NOT be skipped
            TaskNode::new("n4", "a", "D"),
        ]);

        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);
        scheduler.skip_downstream("n1").await.unwrap();

        let g = scheduler.graph.lock().await;
        assert_eq!(g.find_node("n1").unwrap().status, TaskStatus::Pending); // not touched
        assert_eq!(g.find_node("n2").unwrap().status, TaskStatus::Skipped);
        assert_eq!(g.find_node("n3").unwrap().status, TaskStatus::Skipped);
        assert_eq!(g.find_node("n4").unwrap().status, TaskStatus::Pending); // untouched
    }

    #[tokio::test]
    async fn test_skip_downstream_diamond_graph() {
        // Diamond: n1 → n2, n1 → n3, n2 → n4, n3 → n4
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "a", "A"),
            TaskNode::new("n2", "a", "B").with_dependencies(vec!["n1".into()]),
            TaskNode::new("n3", "a", "C").with_dependencies(vec!["n1".into()]),
            TaskNode::new("n4", "a", "D").with_dependencies(vec!["n2".into(), "n3".into()]),
        ]);

        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);
        scheduler.skip_downstream("n1").await.unwrap();

        let g = scheduler.graph.lock().await;
        assert_eq!(g.find_node("n2").unwrap().status, TaskStatus::Skipped);
        assert_eq!(g.find_node("n3").unwrap().status, TaskStatus::Skipped);
        assert_eq!(g.find_node("n4").unwrap().status, TaskStatus::Skipped);
    }

    #[tokio::test]
    async fn test_build_upstream_context_block_empty_when_no_deps() {
        let graph = TaskGraph::new(vec![TaskNode::new("n1", "a", "A")]);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/upstream-empty"), graph);
        let node = scheduler
            .graph
            .lock()
            .await
            .find_node("n1")
            .unwrap()
            .clone();
        let block = scheduler.build_upstream_context_block(&node).await;
        assert!(block.is_empty());
    }

    #[tokio::test]
    async fn test_build_upstream_context_block_shows_outputs() {
        let graph = TaskGraph::new(vec![
            TaskNode::new("n1", "a", "A"),
            TaskNode::new("n2", "a", "B").with_dependencies(vec!["n1".into()]),
        ]);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/upstream"), graph);
        let mut outputs = serde_json::Map::new();
        outputs.insert(
            "key1".to_string(),
            serde_json::Value::String("value1".to_string()),
        );
        scheduler
            .record_outputs("n1", serde_json::Value::Object(outputs))
            .await;

        let node = scheduler
            .graph
            .lock()
            .await
            .find_node("n2")
            .unwrap()
            .clone();
        let block = scheduler.build_upstream_context_block(&node).await;
        assert!(block.contains("Upstream Context"));
        assert!(block.contains("key1"));
        assert!(block.contains("value1"));
    }

    #[tokio::test]
    async fn test_load_from_disk_roundtrip() {
        let graph = sample_graph();
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".ergatai")).unwrap();

        let scheduler = DagScheduler::new(temp_dir.path().to_path_buf(), graph);
        scheduler.set_global("k", "v").await;
        scheduler.save_graph_unlocked().await.unwrap();

        // Reload
        let loaded = DagScheduler::load_from_disk(temp_dir.path().to_path_buf())
            .await
            .unwrap();
        assert!(!loaded.is_complete().await);
        let ctx = loaded.context();
        let ctx = ctx.lock().await;
        assert_eq!(ctx.get_global("k"), Some("v"));
    }

    #[tokio::test]
    async fn test_status_prompt_returns_non_empty() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/status"), graph);
        let prompt = scheduler.status_prompt().await;
        assert!(!prompt.is_empty());
    }

    #[tokio::test]
    async fn dag_budget_exhausted_returns_error() {
        // Setup: construct a DagScheduler with max_agent_calls = Some(1).
        let mut graph = sample_graph();
        graph.max_agent_calls = Some(1);
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/budget"), graph);

        // First budget check should succeed (counter = 0, limit = 1).
        assert!(
            scheduler.check_budget().is_ok(),
            "check_budget should be Ok before any calls"
        );

        // Consume the single allowed agent call.
        let new_count = scheduler.increment_agent_calls();
        assert_eq!(new_count, 1, "first increment should yield 1");

        // Second budget check should fail with "budget exhausted" in message.
        let err = scheduler
            .check_budget()
            .expect_err("check_budget should be Err after exhausting the budget");
        let msg = err.to_string();
        assert!(
            msg.contains("budget exhausted"),
            "expected 'budget exhausted' in error message, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn dag_deadline_check_returns_reason_when_expired() {
        let graph = sample_graph();
        let mut scheduler = DagScheduler::new(PathBuf::from("/tmp/deadline"), graph);
        // No deadline set → should return None
        assert!(scheduler.check_deadline().is_none());
        // Set deadline to past
        scheduler.deadline =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(10));
        // Should return Some(reason) with "exceeded deadline" in message
        let reason = scheduler.check_deadline().unwrap();
        assert!(
            reason.contains("exceeded deadline"),
            "expected 'exceeded deadline' in reason, got: {}",
            reason
        );
        // Reason should also include the dag_id for observability
        assert!(
            reason.contains(scheduler.dag_id()),
            "expected dag_id in reason, got: {}",
            reason
        );
    }

    #[tokio::test]
    async fn last_progress_initializes_grows_and_refreshes() {
        let graph = sample_graph();
        let scheduler = DagScheduler::new(PathBuf::from("/tmp/progress-ts"), graph);

        // 1. Immediately after construction, age should be small (< 2s).
        let initial = scheduler.last_progress_age_secs().await;
        assert!(
            initial < 2,
            "initial last_progress age should be < 2s, got {}s",
            initial
        );

        // 2. Wait a bit, then age should have grown to >= 1s.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let grown = scheduler.last_progress_age_secs().await;
        assert!(
            grown >= 1,
            "last_progress age should have grown to >= 1s after sleeping, got {}s",
            grown
        );

        // 3. touch_progress resets the timestamp; age should be small again.
        scheduler.touch_progress().await;
        let refreshed = scheduler.last_progress_age_secs().await;
        assert!(
            refreshed < 2,
            "last_progress age should be < 2s after touch_progress, got {}s",
            refreshed
        );
    }
}
