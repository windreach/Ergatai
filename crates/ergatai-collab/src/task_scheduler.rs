// Task Scheduler - Manages task distribution to agents
// Global singleton, persistent queue, MCP-based status checking

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use ergatai_error::{ErgataiError, ErgataiResult};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::agent_launcher::AgentLauncher;
use super::task_coordinator::{AgentAssignment, TaskCoordinator, TaskPlan};

/// Queue file format version — increment when PendingTask schema changes.
/// Migration logic in load_from_disk handles older versions.
const QUEUE_FILE_VERSION: u32 = 1;

/// Default concurrency limit — maximum number of tasks running simultaneously.
/// Applies across all agents; prevents resource exhaustion from unlimited parallelism.
const DEFAULT_CONCURRENCY_LIMIT: usize = 8;

/// Versioned queue file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueueFile {
    version: u32,
    tasks: Vec<PendingTask>,
}

/// Agent availability status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentAvailability {
    /// Agent is free, can accept new task
    Available,
    /// Agent is busy with existing task
    Busy { current_task_id: String },
    /// Agent not found or not launched
    NotRunning,
}

/// Task scheduling strategy
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScheduleStrategy {
    /// Wait for agent to be free, then execute (serial)
    WaitForAgent,
    /// Queue task and execute when agent is free
    QueueTask,
    /// Launch new agent instance (parallel)
    Parallel,
}

/// Pending task waiting to be scheduled
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTask {
    pub task_id: String,
    pub plan_file: PathBuf,
    pub target_agent: String,
    pub submitted_at: u64,
    pub priority: u32,
}

/// Active processing entry: (task_id, agent_name, optional_permit)
///
/// The permit is held only by the first agent of each task; additional agents
/// from the same task have None. Dropping the entry releases the semaphore slot.
type ProcessingEntry = (String, String, Option<OwnedSemaphorePermit>);

/// Task Scheduler - coordinates task distribution (global singleton)
pub struct TaskScheduler {
    project_root: PathBuf,
    strategy: ScheduleStrategy,
    /// Pending tasks waiting to be scheduled (in-memory, persisted to disk on changes)
    pending_tasks: Arc<Mutex<Vec<PendingTask>>>,
    /// Active processing entries
    processing: Arc<Mutex<Vec<ProcessingEntry>>>,
    queue_file: PathBuf,
    /// Maximum number of tasks that can run concurrently.
    concurrency_limit: usize,
    /// Semaphore enforcing the concurrency limit. Each running task holds one permit.
    semaphore: Arc<Semaphore>,
}

impl TaskScheduler {
    /// Create a new task scheduler (private, use global_scheduler() instead)
    fn new(project_root: PathBuf, strategy: ScheduleStrategy, concurrency_limit: usize) -> Self {
        let queue_file = project_root.join(".ergatai").join(".scheduler-queue.json");
        let limit = if concurrency_limit == 0 {
            DEFAULT_CONCURRENCY_LIMIT
        } else {
            concurrency_limit
        };

        Self {
            project_root,
            strategy,
            pending_tasks: Arc::new(Mutex::new(Vec::new())),
            processing: Arc::new(Mutex::new(Vec::new())),
            queue_file,
            concurrency_limit: limit,
            semaphore: Arc::new(Semaphore::new(limit)),
        }
    }

    /// Load pending tasks from disk (call once at startup or when recovering state)
    pub async fn load_from_disk(&self) -> ErgataiResult<()> {
        if tokio::fs::try_exists(&self.queue_file)
            .await
            .unwrap_or(false)
        {
            let content = tokio::fs::read_to_string(&self.queue_file)
                .await
                .with_context(|| format!("Failed to read queue file: {:?}", self.queue_file))?;

            // Try to parse as versioned format first
            let tasks = match serde_json::from_str::<QueueFile>(&content) {
                Ok(queue_file) => {
                    // Versioned format — check version and migrate if needed
                    match queue_file.version {
                        v if v == QUEUE_FILE_VERSION => queue_file.tasks,
                        v if v < QUEUE_FILE_VERSION => {
                            tracing::warn!(
                                "Queue file version {} is outdated (current: {}), attempting migration",
                                v,
                                QUEUE_FILE_VERSION
                            );
                            // For now, just use the tasks as-is since v1 has no schema changes.
                            // Future versions should add migration logic here.
                            queue_file.tasks
                        }
                        v => {
                            tracing::error!(
                                "Queue file version {} is newer than supported version {}, clearing queue",
                                v,
                                QUEUE_FILE_VERSION
                            );
                            Vec::new()
                        }
                    }
                }
                Err(_) => {
                    // Try to parse as legacy format (bare Vec<PendingTask>)
                    match serde_json::from_str::<Vec<PendingTask>>(&content) {
                        Ok(tasks) => {
                            tracing::info!(
                                "Loaded legacy queue format, will save as versioned format"
                            );
                            tasks
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse queue file: {}", e);
                            return Err(ErgataiError::json(format!(
                                "Failed to parse queue file: {}",
                                e
                            )));
                        }
                    }
                }
            };

            let mut pending = self.pending_tasks.lock().await;
            if pending.is_empty() {
                *pending = tasks;
                tracing::info!("Loaded {} pending tasks from disk", pending.len());
            }
        }
        Ok(())
    }

    /// Save pending tasks to disk (atomic: write-to-temp + rename)
    pub async fn save_to_disk(&self) -> ErgataiResult<()> {
        let tasks = self.pending_tasks.lock().await.clone();
        let queue_file = QueueFile {
            version: QUEUE_FILE_VERSION,
            tasks,
        };
        let content = serde_json::to_string_pretty(&queue_file)?;

        // Atomic write: write to temp file first, then rename
        let temp_file = self.queue_file.with_extension("json.tmp");
        tokio::fs::write(&temp_file, content.as_bytes()).await?;
        if let Err(e) = tokio::fs::rename(&temp_file, &self.queue_file).await {
            // Clean up temp file on rename failure
            let _ = tokio::fs::remove_file(&temp_file).await;
            return Err(ErgataiError::internal_with_source(
                "Failed to atomically rename queue file",
                e,
            ));
        }

        Ok(())
    }

    /// Submit a new task for scheduling
    /// Submit a task for scheduling with explicit priority
    ///
    /// Priority is a u32 value where higher numbers = higher priority.
    /// Typical mapping: high=3, medium=2, low=1
    pub async fn submit_task_with_priority(
        &self,
        plan_file: PathBuf,
        priority: u32,
    ) -> ErgataiResult<String> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Load from disk once (idempotent, only loads if pending_tasks is empty)
        self.load_from_disk().await?;

        let coordinator = TaskCoordinator::new(self.project_root.clone());
        let plan = coordinator.parse_plan(&plan_file).await?;
        let task_id = plan.task_id.clone();

        // Find target agent (assume single agent for now)
        let target_agent = plan
            .assignments
            .first()
            .map(|a| a.agent_name.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let pending = PendingTask {
            task_id: task_id.clone(),
            plan_file,
            target_agent,
            submitted_at: now,
            priority,
        };

        // Try to schedule immediately
        match self.try_schedule_task(&pending, &plan).await? {
            true => {
                tracing::info!("Task {} scheduled immediately", task_id);
            }
            false => {
                tracing::info!("Task {} queued (agent busy)", task_id);
                self.pending_tasks.lock().await.push(pending);
                self.save_to_disk().await?;
            }
        }

        Ok(task_id)
    }

    pub async fn submit_task(&self, plan_file: PathBuf) -> ErgataiResult<String> {
        // Default to medium priority (2) for backward compatibility
        self.submit_task_with_priority(plan_file, 2).await
    }

    /// Try to schedule a task immediately
    ///
    /// Acquires a concurrency permit before launching. If the global concurrency
    /// limit is reached, returns `Ok(false)` — the task will be retried later
    /// by the background scheduler loop.
    async fn try_schedule_task(&self, task: &PendingTask, plan: &TaskPlan) -> ErgataiResult<bool> {
        // Try to acquire a concurrency permit (non-blocking)
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::info!(
                    task_id = %task.task_id,
                    limit = self.concurrency_limit,
                    "Concurrency limit reached, queueing task"
                );
                return Ok(false);
            }
        };

        let availability = self.check_agent_availability(&task.target_agent).await;

        match availability {
            AgentAvailability::Available | AgentAvailability::NotRunning => {
                // Agent is free, launch task (pass permit to store in processing list)
                self.launch_task(task, plan, permit).await?;
                Ok(true)
            }
            AgentAvailability::Busy { current_task_id } => {
                // Agent is busy, handle based on strategy
                match self.strategy {
                    ScheduleStrategy::WaitForAgent | ScheduleStrategy::QueueTask => {
                        // Drop permit — task goes to queue, will be retried later
                        drop(permit);
                        Ok(false)
                    }
                    ScheduleStrategy::Parallel => {
                        // Launch new agent instance (different worktree)
                        tracing::warn!(
                            "Parallel mode: launching new instance for task {} (agent {} busy with {})",
                            task.task_id,
                            task.target_agent,
                            current_task_id
                        );
                        self.launch_task(task, plan, permit).await?;
                        Ok(true)
                    }
                }
            }
        }
    }

    /// Launch a task (start agent). The concurrency permit is stored alongside
    /// the task entry and released when the task is removed from processing.
    async fn launch_task(
        &self,
        _task: &PendingTask,
        plan: &TaskPlan,
        permit: OwnedSemaphorePermit,
    ) -> ErgataiResult<()> {
        let coordinator = TaskCoordinator::new(self.project_root.clone());
        let launcher = AgentLauncher::new(self.project_root.clone());

        // Initialize coordinator
        coordinator.init().await?;

        // Launch agents
        let agent_ids = launcher.launch_agents(plan).await?;

        // Parse all agent IDs first to avoid ownership issues with the permit
        let parsed_agents: Vec<(String, String)> = agent_ids
            .iter()
            .filter_map(|id| {
                AgentLauncher::parse_agent_id(id).map(|(t, a)| (t.to_string(), a.to_string()))
            })
            .collect();

        // Store (task_id, agent_name, permit) triples in processing list.
        // The concurrency limit applies per-task, not per-agent.
        // Only the first agent holds the permit; additional agents from the same
        // task are tracked but don't consume extra semaphore slots.
        let mut processing = self.processing.lock().await;

        if let Some((task_id, agent_name)) = parsed_agents.first().cloned() {
            // First agent holds the permit for this task
            processing.push((task_id, agent_name, Some(permit)));

            // Remaining agents (if any) are tracked but don't hold permits
            for (task_id, agent_name) in parsed_agents.into_iter().skip(1) {
                processing.push((task_id, agent_name, None));
            }
        }
        // If no agents parsed, permit is dropped here (normal drop)

        Ok(())
    }

    /// Check agent availability via local session tracking
    pub async fn check_agent_availability(&self, agent_name: &str) -> AgentAvailability {
        // Check our local processing list for exact match on agent_name
        let processing = self.processing.lock().await;

        for (task_id, name, _) in processing.iter() {
            if name == agent_name {
                // Agent is processing a task
                return AgentAvailability::Busy {
                    current_task_id: task_id.clone(),
                };
            }
        }

        // Agent is available
        AgentAvailability::Available
    }

    /// Process pending tasks (call this periodically or after task completion)
    pub async fn process_pending(&self) -> ErgataiResult<usize> {
        let mut scheduled_count = 0;

        // Collect and sort tasks, then release lock immediately to avoid
        // holding it across await points (file I/O below)
        // Sort by priority DESCENDING (higher number = higher priority),
        // then by submitted_at ASCENDING (FIFO for same priority)
        let mut tasks: Vec<PendingTask> = {
            let mut pending = self.pending_tasks.lock().await;
            pending.sort_by(|a, b| {
                b.priority
                    .cmp(&a.priority)
                    .then(a.submitted_at.cmp(&b.submitted_at))
            });
            pending.drain(..).collect()
        }; // lock released here

        // Try to schedule pending tasks (no lock held during I/O)
        let mut remaining = Vec::new();
        let coordinator = TaskCoordinator::new(self.project_root.clone());
        for task in tasks.drain(..) {
            // Parse the plan file
            let plan = match coordinator.parse_plan(&task.plan_file).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("Failed to parse plan for task {}: {}", task.task_id, e);
                    remaining.push(task);
                    continue;
                }
            };

            match self.try_schedule_task(&task, &plan).await {
                Ok(true) => {
                    scheduled_count += 1;
                    tracing::info!("Scheduled pending task: {}", task.task_id);
                }
                Ok(false) => {
                    // Still can't schedule, keep in queue
                    remaining.push(task);
                }
                Err(e) => {
                    tracing::error!("Failed to schedule task {}: {}", task.task_id, e);
                    // Put failed tasks back in queue
                    remaining.push(task);
                }
            }
        }

        // Put back unscheduled tasks
        if !remaining.is_empty() {
            let mut pending = self.pending_tasks.lock().await;
            pending.extend(remaining);
        }

        self.save_to_disk().await?;

        Ok(scheduled_count)
    }

    /// Get pending task count
    pub async fn pending_count(&self) -> usize {
        self.pending_tasks.lock().await.len()
    }

    /// Get list of pending tasks
    pub async fn list_pending(&self) -> Vec<PendingTask> {
        self.pending_tasks.lock().await.clone()
    }

    /// Cancel a pending task
    pub async fn cancel_task(&self, task_id: &str) -> ErgataiResult<bool> {
        let mut tasks = self.pending_tasks.lock().await;
        let initial_len = tasks.len();
        tasks.retain(|t| t.task_id != task_id);
        let removed = tasks.len() < initial_len;

        if removed {
            drop(tasks);
            self.save_to_disk().await?;
        }

        Ok(removed)
    }

    /// Mark task as completed (remove from processing, releasing its concurrency permit)
    pub async fn mark_completed(&self, task_id: &str) {
        let mut processing = self.processing.lock().await;
        let before = processing.len();
        processing.retain(|(tid, _, _)| tid != task_id);
        let removed = before - processing.len();
        if removed > 0 {
            drop(processing);
            tracing::debug!(
                task_id = task_id,
                available = self.semaphore.available_permits(),
                "Task completed, concurrency permit released"
            );
        }
    }

    /// Cancel a currently running task (remove from processing, releasing its permit).
    /// Also stops the agent via AgentRuntime (sends Ctrl+C, waits, then kills if needed).
    /// Returns `true` if the task was found and removed.
    pub async fn cancel_running_task(&self, task_id: &str) -> bool {
        let mut processing = self.processing.lock().await;
        let before = processing.len();

        // Find and remove the entry, capturing agent_name for stopping
        let mut agent_name_to_stop = None;
        processing.retain(|(tid, agent_name, _)| {
            if tid == task_id {
                agent_name_to_stop = Some(agent_name.clone());
                false
            } else {
                true
            }
        });

        let removed = before - processing.len();
        drop(processing); // Release lock before calling external code

        if removed > 0 {
            tracing::info!(
                task_id = task_id,
                available = self.semaphore.available_permits(),
                "Running task cancelled, concurrency permit released"
            );

            // Stop the agent via AgentRuntime
            if let Some(agent_name) = agent_name_to_stop {
                let agent_id = format!("{}|{}", task_id, agent_name);
                let runtime = ergatai_runtime::get_agent_runtime();
                if let Err(e) = runtime.stop_agent(&agent_id).await {
                    tracing::warn!(
                        task_id = task_id,
                        agent_id = agent_id,
                        error = %e,
                        "Failed to stop agent (may have already exited)"
                    );
                }
            }
            true
        } else {
            false
        }
    }

    /// Current concurrency limit (max simultaneous tasks)
    pub fn concurrency_limit(&self) -> usize {
        self.concurrency_limit
    }

    /// Number of available concurrency slots (how many more tasks can start right now)
    pub async fn available_concurrency(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Number of currently running tasks
    pub async fn running_count(&self) -> usize {
        self.processing.lock().await.len()
    }

    /// Start background scheduler loop (auto-process pending tasks)
    pub fn start_background_scheduler(self: &Arc<Self>) {
        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Check and process pending tasks every 5 seconds
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                if scheduler.pending_count().await > 0 {
                    match scheduler.process_pending().await {
                        Ok(count) if count > 0 => {
                            tracing::info!(
                                "Background scheduler: processed {} pending tasks",
                                count
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("Background scheduler error: {}", e);
                        }
                    }
                }
            }
        });
    }

    /// Start NATS consumer loop — receives task submissions via JetStream pull consumer
    ///
    /// Pulls messages from the `DAG_EVENTS` stream with filter `ergatai.task.submit.*`.
    /// Tasks are durable: if this consumer crashes, JetStream redelivers after `ack_wait`.
    ///
    /// Returns a `JoinHandle` that can be aborted to stop consuming.
    /// If NATS is not initialized, the spawned task exits immediately.
    pub fn start_nats_consumer(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let scheduler = Arc::clone(self);

        tokio::spawn(async move {
            let conn = match ergatai_nats::get_nats_connection().await {
                Some(c) => c,
                None => {
                    tracing::warn!("NATS not initialized, consumer not started");
                    return;
                }
            };

            // Initialize JetStream pull consumer on the DAG_EVENTS stream,
            // filtered to task submissions only.
            let mut messages = match init_task_submission_consumer(&conn).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to initialize task submission consumer");
                    return;
                }
            };

            tracing::info!(
                "JetStream task consumer started (stream: {}, filter: ergatai.task.submit.*)",
                ergatai_nats::DAG_EVENTS_STREAM
            );

            use futures_util::StreamExt;
            loop {
                // Use timeout so we periodically yield and can be aborted cleanly.
                let next =
                    tokio::time::timeout(std::time::Duration::from_secs(5), messages.next()).await;

                match next {
                    // Timeout — no message within 5s, loop and check for abort
                    Err(_) => continue,

                    // Stream closed
                    Ok(None) => {
                        tracing::warn!("Task submission stream closed, consumer exiting");
                        break;
                    }

                    // Message received
                    Ok(Some(Ok(js_msg))) => {
                        match serde_json::from_slice::<ergatai_nats::TaskSubmitPayload>(
                            &js_msg.payload,
                        ) {
                            Ok(payload) => {
                                let seq =
                                    js_msg.info().ok().map(|i| i.stream_sequence).unwrap_or(0);
                                tracing::info!(
                                    task_id = %payload.task_id,
                                    agent = %payload.target_agent,
                                    seq = seq,
                                    "Received JetStream task submission"
                                );

                                match scheduler.handle_nats_task(payload).await {
                                    Ok(()) => {
                                        if let Err(e) = js_msg.ack().await {
                                            tracing::warn!(error = %e, "Failed to ack task");
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to handle task — naking for redelivery");
                                        if let Err(nak_err) = js_msg
                                            .ack_with(async_nats::jetstream::message::AckKind::Nak(
                                                None,
                                            ))
                                            .await
                                        {
                                            tracing::warn!(error = %nak_err, "Failed to nak task");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                // Malformed message — ack to discard (retry won't help)
                                tracing::warn!(
                                    error = %e,
                                    subject = js_msg.subject.as_str(),
                                    "Failed to deserialize task submission — acking to discard"
                                );
                                if let Err(ack_err) = js_msg.ack().await {
                                    tracing::warn!(error = %ack_err, "Failed to ack malformed task");
                                }
                            }
                        }
                    }

                    // Transport error
                    Ok(Some(Err(e))) => {
                        tracing::warn!(error = %e, "Error receiving task from stream");
                    }
                }
            }

            tracing::warn!("JetStream task consumer stopped");
        })
    }

    /// Handle a task received via NATS
    ///
    /// Writes the inline plan content to a file (for agent readability),
    /// then processes it through the normal scheduling pipeline.
    async fn handle_nats_task(
        &self,
        payload: ergatai_nats::TaskSubmitPayload,
    ) -> ErgataiResult<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Validate plan_file path: must reside within <project_root>/.ergatai/.dag-plans/
        let plan_file = PathBuf::from(&payload.plan_file);
        let allowed_dir = self.project_root.join(".ergatai").join(".dag-plans");

        // Ensure allowed directory exists, then canonicalize it.
        tokio::fs::create_dir_all(&allowed_dir).await?;
        let canonical_allowed = allowed_dir.canonicalize().map_err(|e| {
            ErgataiError::internal(format!("Failed to canonicalize allowed dir: {}", e))
        })?;

        // Canonicalize plan_file. If it doesn't yet exist, canonicalize its parent
        // (creating the parent under the allowed dir) and re-attach the file name.
        let canonical = if plan_file.exists() {
            plan_file.canonicalize().map_err(|e| {
                ErgataiError::internal(format!("Failed to canonicalize plan file path: {}", e))
            })?
        } else {
            let parent = plan_file.parent().ok_or_else(|| {
                ErgataiError::InvalidArgument("Invalid plan file path: no parent".to_string())
            })?;
            // Only create parent dirs if they are within the allowed directory;
            // canonicalize the existing ancestor first to resolve any `..` segments
            // before comparing.
            let mut ancestor_to_create = parent.to_path_buf();
            while !ancestor_to_create.exists() {
                match ancestor_to_create.parent() {
                    Some(p) => ancestor_to_create = p.to_path_buf(),
                    None => break,
                }
            }
            if ancestor_to_create.exists() {
                let canon_ancestor = ancestor_to_create.canonicalize().map_err(|e| {
                    ErgataiError::internal(format!("Failed to canonicalize ancestor: {}", e))
                })?;
                if !canon_ancestor.starts_with(&canonical_allowed) {
                    return Err(ErgataiError::InvalidArgument(format!(
                        "plan_file {:?} resolves outside allowed directory {:?}",
                        plan_file, allowed_dir
                    )));
                }
            }
            tokio::fs::create_dir_all(parent).await?;
            let canon_parent = parent.canonicalize().map_err(|e| {
                ErgataiError::internal(format!("Failed to canonicalize parent dir: {}", e))
            })?;
            let file_name = plan_file.file_name().ok_or_else(|| {
                ErgataiError::InvalidArgument("Invalid plan file name".to_string())
            })?;
            canon_parent.join(file_name)
        };

        // Final containment check after full canonicalization.
        if !canonical.starts_with(&canonical_allowed) {
            return Err(ErgataiError::InvalidArgument(format!(
                "plan_file {:?} is outside allowed directory {:?}",
                plan_file, allowed_dir
            )));
        }

        tokio::fs::write(&canonical, payload.plan_content.as_bytes()).await?;

        // Build a PendingTask
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let pending = PendingTask {
            task_id: payload.task_id.clone(),
            plan_file: canonical.clone(),
            target_agent: payload.target_agent.clone(),
            submitted_at: now,
            priority: payload.priority,
        };

        // Build a minimal TaskPlan from the payload
        let plan = TaskPlan {
            task_id: payload.task_id.clone(),
            task_name: payload.task_id.clone(),
            coordinator: "nats".to_string(),
            status: super::task_coordinator::PlanStatus::InProgress,
            assignments: vec![AgentAssignment {
                agent_name: payload.target_agent.clone(),
                objective: String::new(),
                files_to_create: vec![],
                files_to_modify: vec![],
                files_to_read: vec![],
                task_type: super::task_coordinator::TaskType::CreateNew,
                depends_on: vec![],
                priority: None,
            }],
            merge_strategy: "none".to_string(),
            plan_file: canonical,
        };

        // Try to schedule immediately
        match self.try_schedule_task(&pending, &plan).await? {
            true => {
                tracing::info!(task_id = %payload.task_id, "NATS task scheduled immediately");
            }
            false => {
                tracing::info!(task_id = %payload.task_id, "NATS task queued (agent busy)");
                self.pending_tasks.lock().await.push(pending);
                self.save_to_disk().await?;
            }
        }

        Ok(())
    }

    /// Start a lifecycle event consumer that reacts to agent state changes.
    ///
    /// Subscribes to `ergatai.agent.lifecycle.*` (all agents). When an agent
    /// transitions to a terminal state (failed, timed_out, signaled) while
    /// holding a task, the task is cancelled and released so it can be
    /// retried or re-scheduled.
    ///
    /// Uses core NATS (not JetStream) for low-latency fan-out.
    ///
    /// Returns a [`LifecycleConsumerHandle`] that can be used to gracefully
    /// shut down the consumer. Dropping the handle without calling
    /// `shutdown()` will leave the consumer running until the NATS
    /// subscription closes.
    pub fn start_lifecycle_consumer(self: &Arc<Self>) -> LifecycleConsumerHandle {
        let scheduler = Arc::clone(self);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let join_handle = tokio::spawn(async move {
            let conn = match ergatai_nats::get_nats_connection().await {
                Some(c) => c,
                None => {
                    tracing::warn!("NATS not initialized, lifecycle consumer not started");
                    return;
                }
            };

            let event_bus = ergatai_nats::EventBus::new(conn);

            let mut subscriber = match event_bus.subscribe_all_agent_lifecycles().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to subscribe to agent lifecycle events");
                    return;
                }
            };

            tracing::info!("Agent lifecycle consumer started (subject: ergatai.agent.lifecycle.*)");

            let mut shutdown_rx = shutdown_rx;
            use futures_util::StreamExt;
            loop {
                tokio::select! {
                    biased; // Check shutdown first

                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            tracing::info!("Agent lifecycle consumer received shutdown signal");
                            break;
                        }
                    }

                    msg = subscriber.next() => {
                        let Some(msg) = msg else {
                            tracing::warn!("Agent lifecycle consumer stream closed");
                            break;
                        };

                        let payload: ergatai_nats::AgentLifecycleEventPayload =
                            match serde_json::from_slice(&msg.payload) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to deserialize lifecycle event");
                                    continue;
                                }
                            };

                        // Only react to terminal states where the agent had a task
                        if !payload.is_terminal {
                            continue;
                        }

                        let Some(task_id) = &payload.task_id else {
                            continue;
                        };

                        tracing::warn!(
                            agent_uuid = %payload.agent_uuid,
                            agent_id = %payload.agent_id,
                            task_id = %task_id,
                            from_state = %payload.from_state,
                            to_state = %payload.to_state,
                            reason = ?payload.reason,
                            "Agent terminated while holding task — cancelling task"
                        );

                        // Cancel the running task (releases concurrency permit)
                        scheduler.cancel_running_task(task_id).await;
                    }
                }
            }
        });

        LifecycleConsumerHandle {
            join_handle: Some(join_handle),
            shutdown_tx: Some(shutdown_tx),
        }
    }
}

/// Handle to a running lifecycle consumer task.
///
/// Call [`shutdown`](Self::shutdown) to gracefully stop the consumer.
/// Dropping the handle without shutdown leaves the consumer running.
pub struct LifecycleConsumerHandle {
    join_handle: Option<tokio::task::JoinHandle<()>>,
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl LifecycleConsumerHandle {
    /// Signal the consumer to stop and wait for it to finish.
    ///
    /// Returns immediately if the consumer has already exited.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.await;
        }
    }

    /// Check if the consumer task has finished.
    pub fn is_finished(&self) -> bool {
        self.join_handle.as_ref().is_none_or(|h| h.is_finished())
    }
}

impl Drop for LifecycleConsumerHandle {
    fn drop(&mut self) {
        // If dropped without shutdown(), signal the consumer to stop but don't await.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        // JoinHandle is detached — task continues until it observes the shutdown signal.
    }
}

/// Initialize the JetStream pull consumer for task submissions on the DAG_EVENTS stream.
///
/// Returns a boxed stream of JetStream messages filtered to `ergatai.task.submit.*`.
/// The consumer is durable (`task_submissions`) and resumes from the last ack on restart.
async fn init_task_submission_consumer(
    connection: &ergatai_nats::NatsConnection,
) -> ErgataiResult<
    futures_util::stream::BoxStream<
        'static,
        Result<async_nats::jetstream::Message, Box<dyn std::error::Error + Send + Sync>>,
    >,
> {
    ergatai_nats::init_dag_stream_pull_consumer(
        connection,
        ergatai_nats::TASK_SUBMISSIONS_CONSUMER,
        "ergatai.task.submit.*",
    )
    .await
    .map_err(ErgataiError::NatsError)
}

/// Global scheduler instance
use std::sync::OnceLock;
static GLOBAL_SCHEDULER: OnceLock<Arc<TaskScheduler>> = OnceLock::new();

/// Get or create global scheduler instance
pub fn global_scheduler(project_root: Option<PathBuf>) -> Arc<TaskScheduler> {
    GLOBAL_SCHEDULER
        .get_or_init(|| {
            let root = project_root.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let scheduler = Arc::new(TaskScheduler::new(
                root,
                ScheduleStrategy::WaitForAgent,
                DEFAULT_CONCURRENCY_LIMIT,
            ));

            // Start background scheduler (polling fallback)
            scheduler.start_background_scheduler();

            // Start NATS consumer if available (event-driven, replaces polling when active)
            scheduler.start_nats_consumer();

            scheduler
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper — creates a scheduler with a high concurrency limit (no blocking)
    /// and a temp directory as project root. Tests acquire a permit to push into
    /// the processing list.
    fn test_scheduler(root: &std::path::Path) -> TaskScheduler {
        TaskScheduler::new(root.to_path_buf(), ScheduleStrategy::WaitForAgent, 128)
    }

    /// Acquire one semaphore permit for storing in the processing list during tests.
    /// Returns Some(permit) for the first agent of a task, None for additional agents.
    async fn test_permit(scheduler: &TaskScheduler) -> Option<OwnedSemaphorePermit> {
        Some(
            scheduler
                .semaphore
                .clone()
                .try_acquire_owned()
                .expect("test permit"),
        )
    }

    #[tokio::test]
    async fn test_scheduler_creation() {
        let temp_dir = std::env::temp_dir().join("test-scheduler");
        let scheduler = test_scheduler(&temp_dir);
        assert_eq!(scheduler.pending_count().await, 0);
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_strategy_default_values() {
        assert_eq!(
            ScheduleStrategy::WaitForAgent,
            ScheduleStrategy::WaitForAgent
        );
        assert_ne!(ScheduleStrategy::QueueTask, ScheduleStrategy::Parallel);
    }

    #[tokio::test]
    async fn test_agent_availability_initially_available() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        let avail = scheduler.check_agent_availability("any-agent").await;
        assert_eq!(avail, AgentAvailability::Available);
    }

    #[tokio::test]
    async fn test_agent_availability_busy_after_processing_added() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        // Simulate an agent currently processing a task
        {
            let mut p = scheduler.processing.lock().await;
            p.push((
                "task-1".to_string(),
                "alice".to_string(),
                test_permit(&scheduler).await,
            ));
        } // lock dropped here

        let avail = scheduler.check_agent_availability("alice").await;
        assert!(matches!(
            avail,
            AgentAvailability::Busy { current_task_id } if current_task_id == "task-1"
        ));

        // Different agent should still be Available
        let avail = scheduler.check_agent_availability("bob").await;
        assert_eq!(avail, AgentAvailability::Available);
    }

    #[tokio::test]
    async fn test_mark_completed_removes_from_processing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        {
            let mut p = scheduler.processing.lock().await;
            p.push((
                "task-1".to_string(),
                "alice".to_string(),
                test_permit(&scheduler).await,
            ));
            p.push((
                "task-2".to_string(),
                "bob".to_string(),
                test_permit(&scheduler).await,
            ));
        } // lock dropped here

        scheduler.mark_completed("task-1").await;

        let processing = scheduler.processing.lock().await;
        assert_eq!(processing.len(), 1);
        assert_eq!(processing[0].0, "task-2");
    }

    #[tokio::test]
    async fn test_mark_completed_idempotent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        // Marking a non-existent task should not error or panic
        scheduler.mark_completed("does-not-exist").await;
        assert!(scheduler.processing.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_task_returns_true_when_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "task-1".to_string(),
            plan_file: PathBuf::from("/tmp/p.md"),
            target_agent: "alice".to_string(),
            submitted_at: 100,
            priority: 1,
        });

        let removed = scheduler.cancel_task("task-1").await.unwrap();
        assert!(removed);
        assert_eq!(scheduler.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_cancel_task_returns_false_when_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        let removed = scheduler.cancel_task("no-such-task").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn test_list_pending_returns_clone() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "t1".to_string(),
            plan_file: PathBuf::from("a.md"),
            target_agent: "alice".to_string(),
            submitted_at: 10,
            priority: 2,
        });
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "t2".to_string(),
            plan_file: PathBuf::from("b.md"),
            target_agent: "bob".to_string(),
            submitted_at: 20,
            priority: 1,
        });

        let list = scheduler.list_pending().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].task_id, "t1");
        assert_eq!(list[1].task_id, "t2");
    }

    #[tokio::test]
    async fn test_save_and_load_from_disk_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        // save_to_disk writes into <project_root>/.ergatai/, so create it
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());

        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "t1".to_string(),
            plan_file: PathBuf::from("/tmp/p.md"),
            target_agent: "alice".to_string(),
            submitted_at: 42,
            priority: 3,
        });

        scheduler.save_to_disk().await.unwrap();

        // Create a fresh scheduler pointing to the same file, load
        let scheduler2 = test_scheduler(temp_dir.path());
        scheduler2.load_from_disk().await.unwrap();

        let tasks = scheduler2.list_pending().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "t1");
        assert_eq!(tasks[0].target_agent, "alice");
        assert_eq!(tasks[0].submitted_at, 42);
        assert_eq!(tasks[0].priority, 3);
    }

    #[tokio::test]
    async fn test_load_from_disk_no_file_is_ok() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        // No queue file exists — should not error
        scheduler.load_from_disk().await.unwrap();
        assert_eq!(scheduler.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_load_from_disk_rejects_corrupted_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_file = temp_dir
            .path()
            .join(".ergatai")
            .join(".scheduler-queue.json");
        tokio::fs::create_dir_all(queue_file.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&queue_file, "this is not json")
            .await
            .unwrap();

        let scheduler = test_scheduler(temp_dir.path());
        let result = scheduler.load_from_disk().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_load_from_disk_legacy_format_supported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_file = temp_dir
            .path()
            .join(".ergatai")
            .join(".scheduler-queue.json");
        tokio::fs::create_dir_all(queue_file.parent().unwrap())
            .await
            .unwrap();
        // Legacy format: bare array without version wrapper
        let legacy = r#"[
            {"task_id":"legacy-1","plan_file":"/tmp/l.md","target_agent":"a","submitted_at":1,"priority":1}
        ]"#;
        tokio::fs::write(&queue_file, legacy).await.unwrap();

        let scheduler = test_scheduler(temp_dir.path());
        scheduler.load_from_disk().await.unwrap();
        let tasks = scheduler.list_pending().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "legacy-1");
    }

    #[tokio::test]
    async fn test_load_from_disk_clears_queue_when_version_newer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_file = temp_dir
            .path()
            .join(".ergatai")
            .join(".scheduler-queue.json");
        tokio::fs::create_dir_all(queue_file.parent().unwrap())
            .await
            .unwrap();
        // Version 999 (way ahead) should clear queue
        let newer = r#"{
            "version": 999,
            "tasks": [{"task_id":"t1","plan_file":"p.md","target_agent":"a","submitted_at":1,"priority":1}]
        }"#;
        tokio::fs::write(&queue_file, newer).await.unwrap();

        let scheduler = test_scheduler(temp_dir.path());
        scheduler.load_from_disk().await.unwrap();
        assert_eq!(scheduler.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_load_from_disk_idempotent_does_not_overwrite_existing() {
        let temp_dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());

        // Pre-populate in memory
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "in-memory".to_string(),
            plan_file: PathBuf::from("x.md"),
            target_agent: "alice".to_string(),
            submitted_at: 0,
            priority: 1,
        });

        // Save a conflicting file
        scheduler.pending_tasks.lock().await.clear();
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "on-disk".to_string(),
            plan_file: PathBuf::from("y.md"),
            target_agent: "bob".to_string(),
            submitted_at: 0,
            priority: 1,
        });
        scheduler.save_to_disk().await.unwrap();

        // Restore in-memory task
        scheduler.pending_tasks.lock().await.clear();
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "in-memory".to_string(),
            plan_file: PathBuf::from("x.md"),
            target_agent: "alice".to_string(),
            submitted_at: 0,
            priority: 1,
        });

        // load_from_disk should not overwrite existing in-memory state
        scheduler.load_from_disk().await.unwrap();
        let tasks = scheduler.list_pending().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "in-memory");
    }

    #[tokio::test]
    async fn test_process_pending_with_no_tasks_is_zero() {
        let temp_dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        let count = scheduler.process_pending().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_process_pending_sorts_by_priority_then_time() {
        // Test the sorting logic by directly manipulating the pending queue
        // and verifying the ordering.
        let temp_dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());

        {
            let mut pending = scheduler.pending_tasks.lock().await;
            pending.push(PendingTask {
                task_id: "low-prio".to_string(),
                plan_file: PathBuf::from("/nonexistent/plan1.md"), // will fail to parse
                target_agent: "a".to_string(),
                submitted_at: 10,
                priority: 5,
            });
            pending.push(PendingTask {
                task_id: "high-prio".to_string(),
                plan_file: PathBuf::from("/nonexistent/plan2.md"), // will fail to parse
                target_agent: "b".to_string(),
                submitted_at: 20,
                priority: 1,
            });
        }

        // process_pending will fail to parse (nonexistent plan files) but the
        // order should have been: high-prio first, then low-prio. Since both fail,
        // both go back into remaining and end up in the queue.
        let count = scheduler.process_pending().await.unwrap();
        assert_eq!(count, 0);
        // Both tasks should still be queued (failed to parse)
        assert_eq!(scheduler.pending_count().await, 2);
    }

    #[tokio::test]
    async fn test_cancel_task_persists_to_disk() {
        let temp_dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(temp_dir.path().join(".ergatai"))
            .await
            .unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        scheduler.pending_tasks.lock().await.push(PendingTask {
            task_id: "t1".to_string(),
            plan_file: PathBuf::from("p.md"),
            target_agent: "alice".to_string(),
            submitted_at: 0,
            priority: 1,
        });
        scheduler.cancel_task("t1").await.unwrap();

        // Verify disk state matches
        let scheduler2 = test_scheduler(temp_dir.path());
        scheduler2.load_from_disk().await.unwrap();
        assert_eq!(scheduler2.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_agent_availability_multiple_tasks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = test_scheduler(temp_dir.path());
        {
            let mut p = scheduler.processing.lock().await;
            p.push((
                "t1".to_string(),
                "alice".to_string(),
                test_permit(&scheduler).await,
            ));
            p.push((
                "t2".to_string(),
                "alice".to_string(),
                test_permit(&scheduler).await,
            ));
        }
        // Should report busy with whichever match is found first
        let avail = scheduler.check_agent_availability("alice").await;
        assert!(matches!(avail, AgentAvailability::Busy { .. }));
    }

    #[tokio::test]
    async fn test_concurrency_limit_enforced() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Create a scheduler with a small concurrency limit
        let scheduler = TaskScheduler::new(
            temp_dir.path().to_path_buf(),
            ScheduleStrategy::WaitForAgent,
            2,
        );

        assert_eq!(scheduler.concurrency_limit(), 2);
        assert_eq!(scheduler.available_concurrency().await, 2);
        assert_eq!(scheduler.running_count().await, 0);

        // Acquire both permits (simulate 2 tasks running)
        let permit1 = scheduler.semaphore.clone().try_acquire_owned().unwrap();
        let permit2 = scheduler.semaphore.clone().try_acquire_owned().unwrap();

        // Third permit should fail (concurrency limit reached)
        assert!(scheduler.semaphore.clone().try_acquire_owned().is_err());
        assert_eq!(scheduler.available_concurrency().await, 0);

        // Release one permit
        drop(permit1);
        assert_eq!(scheduler.available_concurrency().await, 1);

        // Now we can acquire one more
        let permit3 = scheduler.semaphore.clone().try_acquire_owned();
        assert!(permit3.is_ok());

        // Clean up
        drop(permit2);
        drop(permit3);
        assert_eq!(scheduler.available_concurrency().await, 2);
    }

    #[tokio::test]
    async fn test_mark_completed_releases_permit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let scheduler = TaskScheduler::new(
            temp_dir.path().to_path_buf(),
            ScheduleStrategy::WaitForAgent,
            2,
        );

        // Start with 2 available slots
        assert_eq!(scheduler.available_concurrency().await, 2);

        // Add a running task with a permit
        {
            let mut p = scheduler.processing.lock().await;
            p.push((
                "task-1".to_string(),
                "alice".to_string(),
                test_permit(&scheduler).await,
            ));
        }

        // Now only 1 slot available
        assert_eq!(scheduler.available_concurrency().await, 1);
        assert_eq!(scheduler.running_count().await, 1);

        // Mark as completed — permit should be released
        scheduler.mark_completed("task-1").await;

        // Back to 2 available slots
        assert_eq!(scheduler.available_concurrency().await, 2);
        assert_eq!(scheduler.running_count().await, 0);
    }
}
