// Task Scheduler - Manages task distribution to agents
// Global singleton, persistent queue, ACP-based status checking

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use crate::error::{ErgataiError, ErgataiResult};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::task_coordinator::{TaskCoordinator, TaskPlan, AgentAssignment};
use super::agent_launcher::AgentLauncher;

/// Queue file format version — increment when PendingTask schema changes.
/// Migration logic in load_from_disk handles older versions.
const QUEUE_FILE_VERSION: u32 = 1;

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

/// Task Scheduler - coordinates task distribution (global singleton)
pub struct TaskScheduler {
    project_root: PathBuf,
    strategy: ScheduleStrategy,
    /// Pending tasks waiting to be scheduled (in-memory, persisted to disk on changes)
    pending_tasks: Arc<Mutex<Vec<PendingTask>>>,
    /// Active processing: list of (task_id, agent_name) pairs
    processing: Arc<Mutex<Vec<(String, String)>>>,
    queue_file: PathBuf,
}

impl TaskScheduler {
    /// Create a new task scheduler (private, use global_scheduler() instead)
    fn new(project_root: PathBuf, strategy: ScheduleStrategy) -> Self {
        let queue_file = project_root.join(".ergatai").join(".scheduler-queue.json");

        Self {
            project_root,
            strategy,
            pending_tasks: Arc::new(Mutex::new(Vec::new())),
            processing: Arc::new(Mutex::new(Vec::new())),
            queue_file,
        }
    }

    /// Load pending tasks from disk (call once at startup or when recovering state)
    pub async fn load_from_disk(&self) -> ErgataiResult<()> {
        if tokio::fs::try_exists(&self.queue_file).await.unwrap_or(false) {
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
                            tracing::info!("Loaded legacy queue format, will save as versioned format");
                            tasks
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse queue file: {}", e);
                            return Err(ErgataiError::json(format!("Failed to parse queue file: {}", e)));
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

    /// Save pending tasks to disk
    pub async fn save_to_disk(&self) -> ErgataiResult<()> {
        let tasks = self.pending_tasks.lock().await.clone();
        let queue_file = QueueFile {
            version: QUEUE_FILE_VERSION,
            tasks,
        };
        let content = serde_json::to_string_pretty(&queue_file)?;
        tokio::fs::write(&self.queue_file, content).await?;
        Ok(())
    }

    /// Submit a new task for scheduling
    pub async fn submit_task(&self, plan_file: PathBuf) -> ErgataiResult<String> {
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
            priority: 1,
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

    /// Try to schedule a task immediately
    async fn try_schedule_task(&self, task: &PendingTask, plan: &TaskPlan) -> ErgataiResult<bool> {
        let availability = self.check_agent_availability(&task.target_agent).await;

        match availability {
            AgentAvailability::Available | AgentAvailability::NotRunning => {
                // Agent is free, launch task
                self.launch_task(task, plan).await?;
                Ok(true)
            }
            AgentAvailability::Busy { current_task_id } => {
                // Agent is busy, handle based on strategy
                match self.strategy {
                    ScheduleStrategy::WaitForAgent | ScheduleStrategy::QueueTask => {
                        // Queue task, will be picked up later
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
                        self.launch_task(task, plan).await?;
                        Ok(true)
                    }
                }
            }
        }
    }

    /// Launch a task (start agent)
    async fn launch_task(&self, _task: &PendingTask, plan: &TaskPlan) -> ErgataiResult<()> {
        let coordinator = TaskCoordinator::new(self.project_root.clone());
        let launcher = AgentLauncher::new(self.project_root.clone());

        // Initialize coordinator
        coordinator.init().await?;

        // Launch agents
        let agent_ids = launcher.launch_agents(plan).await?;

        // Track processing: store (task_id, agent_name) pairs
        let mut processing = self.processing.lock().await;
        for agent_id in &agent_ids {
            if let Some((task_id, agent_name)) = AgentLauncher::parse_agent_id(agent_id) {
                processing.push((task_id.to_string(), agent_name.to_string()));
            }
        }

        Ok(())
    }

    /// Check agent availability via local session tracking
    pub async fn check_agent_availability(&self, agent_name: &str) -> AgentAvailability {
        // Check our local processing list for exact match on agent_name
        let processing = self.processing.lock().await;

        for (task_id, name) in processing.iter() {
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
        let mut tasks: Vec<PendingTask> = {
            let mut pending = self.pending_tasks.lock().await;
            pending.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
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

    /// Mark task as completed (remove from processing)
    pub async fn mark_completed(&self, task_id: &str) {
        let mut processing = self.processing.lock().await;
        processing.retain(|(tid, _)| tid != task_id);
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
                            tracing::info!("Background scheduler: processed {} pending tasks", count);
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

    /// Start NATS consumer loop — receives task submissions via NATS events
    ///
    /// When NATS is available, this replaces the 5-second polling loop.
    /// Tasks arrive as `TaskSubmitPayload` messages with inline plan content.
    ///
    /// Returns a `JoinHandle` that can be aborted to stop consuming.
    /// If NATS is not initialized, the spawned task exits immediately.
    pub fn start_nats_consumer(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let scheduler = Arc::clone(self);

        tokio::spawn(async move {
            let conn = match crate::nats::get_nats_connection().await {
                Some(c) => c,
                None => {
                    tracing::warn!("NATS not initialized, consumer not started");
                    return;
                }
            };

            let bus = crate::nats::EventBus::new(conn);

            // Subscribe to ALL task submissions (across all agents)
            let mut sub = match bus.subscribe_all_task_submits().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to subscribe to task submissions");
                    return;
                }
            };

            tracing::info!("NATS task consumer started");

            use futures_util::StreamExt;
            while let Some(nats_msg) = sub.next().await {
                match serde_json::from_slice::<crate::nats::TaskSubmitPayload>(&nats_msg.payload) {
                    Ok(payload) => {
                        tracing::info!(
                            task_id = %payload.task_id,
                            agent = %payload.target_agent,
                            "Received NATS task submission"
                        );

                        if let Err(e) = scheduler.handle_nats_task(payload).await {
                            tracing::error!(error = %e, "Failed to handle NATS task");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to deserialize task submission");
                    }
                }
            }

            tracing::warn!("NATS task consumer subscription closed");
        })
    }

    /// Handle a task received via NATS
    ///
    /// Writes the inline plan content to a file (for agent readability),
    /// then processes it through the normal scheduling pipeline.
    async fn handle_nats_task(&self, payload: crate::nats::TaskSubmitPayload) -> ErgataiResult<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Write plan file from inline content (agents read files, not NATS messages)
        let plan_file = PathBuf::from(&payload.plan_file);
        if let Some(parent) = plan_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&plan_file, payload.plan_content.as_bytes()).await?;

        // Build a PendingTask
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let pending = PendingTask {
            task_id: payload.task_id.clone(),
            plan_file: plan_file.clone(),
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
                worktree_name: payload.task_id.clone(),
                depends_on: vec![],
            }],
            merge_strategy: "none".to_string(),
            plan_file,
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
}

/// Global scheduler instance
use std::sync::OnceLock;
static GLOBAL_SCHEDULER: OnceLock<Arc<TaskScheduler>> = OnceLock::new();

/// Get or create global scheduler instance
pub fn global_scheduler(project_root: Option<PathBuf>) -> Arc<TaskScheduler> {
    GLOBAL_SCHEDULER.get_or_init(|| {
        let root = project_root.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let scheduler = Arc::new(TaskScheduler::new(root, ScheduleStrategy::WaitForAgent));

        // Start background scheduler (polling fallback)
        scheduler.start_background_scheduler();

        // Start NATS consumer if available (event-driven, replaces polling when active)
        scheduler.start_nats_consumer();

        scheduler
    }).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scheduler_creation() {
        let temp_dir = std::env::temp_dir().join("test-scheduler");
        let scheduler = TaskScheduler::new(temp_dir.clone(), ScheduleStrategy::WaitForAgent);
        assert_eq!(scheduler.pending_count().await, 0);
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
