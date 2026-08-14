//! SDK-based Agent Pool Manager — manages multiple SDK session instances for concurrent task execution.
//!
//! Maintains high-level abstractions: agent pool, task queue, load balancing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::manager::{event_tx, manager as session_manager, SessionCommand, SessionEvent};
use ergatai_agent::config::AgentConfig;
use ergatai_error::{ErgataiError, ErgataiResult};
use ergatai_nats::{get_nats_connection, NatsTaskQueue};

/// Global task ID counter.
static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Pool-level timeouts.
const PROMPT_MAX_DURATION: Duration = Duration::from_secs(7200); // 2 hours

// ── Pool Types ──

/// Pool status returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct NapiPoolStatus {
    pub agent_name: String,
    pub pool_size: u32,
    pub idle_agents: u32,
    pub busy_agents: u32,
    pub pending_tasks: u32,
    pub in_flight_tasks: u32,
}

/// Pool running status.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum PoolStatus {
    Running,
    Stopped,
}

/// Pool info for listing.
#[derive(Debug, Clone, Serialize)]
pub struct NapiPoolInfo {
    pub agent_name: String,
    pub pool_size: u32,
    pub status: PoolStatus,
}

/// Task info returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct NapiTaskInfo {
    pub task_id: String,
    pub agent_name: String,
    pub status: String, // "pending" | "running" | "completed" | "failed" | "cancelled"
    pub prompt_preview: String,
}

// ── Internal Types ──

/// A single agent instance in the pool (using SDK session).
struct PoolAgent {
    session_id: String,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    busy: bool,
    current_task_id: Option<String>,
}

/// A pending task in the queue.
struct PendingTask {
    task_id: String,
    prompt: String,
}

/// Serializable task payload for NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTaskPayload {
    task_id: String,
    prompt: String,
    agent_name: String,
}

/// Commands sent to the pool's event loop.
enum PoolCommand {
    SubmitTask {
        prompt: String,
        cwd: String,
        reply_tx: oneshot::Sender<Result<String, String>>,
    },
    CancelTask {
        task_id: String,
    },
    GetStatus {
        reply_tx: oneshot::Sender<NapiPoolStatus>,
    },
    Shutdown,
}

/// Handle to a running pool.
struct PoolHandle {
    agent_name: String,
    pool_size: usize,
    cmd_tx: mpsc::UnboundedSender<PoolCommand>,
}

// ── Global Pool Manager ──

/// Global pool manager — singleton.
struct GlobalPoolManager {
    pools: RwLock<HashMap<String, PoolHandle>>,
}

static POOL_MANAGER: std::sync::OnceLock<GlobalPoolManager> = std::sync::OnceLock::new();

fn pool_manager() -> &'static GlobalPoolManager {
    POOL_MANAGER.get_or_init(|| GlobalPoolManager {
        pools: RwLock::new(HashMap::new()),
    })
}

// ── Pool Functions ──

/// Create an agent pool with N concurrent SDK session instances.
pub async fn acp_pool_create(agent_name: String, pool_size: u32, cwd: String) -> ErgataiResult<()> {
    let config = ergatai_agent::config::get_agent_config(&agent_name)
        .map_err(|e| ErgataiError::internal(format!("Failed to load agent config: {}", e)))?;

    let pool_size = pool_size.max(1) as usize;
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (completion_tx, completion_rx) = mpsc::unbounded_channel();

    let handle = PoolHandle {
        agent_name: agent_name.clone(),
        pool_size,
        cmd_tx,
    };

    // Atomically check-and-insert under a single write lock to prevent TOCTOU:
    // a concurrent create for the same agent_name would otherwise both spawn
    // event loops, with the second insert leaking the first one.
    {
        let mut pools = pool_manager().pools.write().await;
        if pools.contains_key(&agent_name) {
            return Err(ErgataiError::internal(format!(
                "Pool already exists for agent: {}",
                agent_name
            )));
        }
        pools.insert(agent_name.clone(), handle);
    }

    // Spawn event loop AFTER the check+insert succeeds. If the check above
    // fails, channels are dropped here without spawning anything — no leak.
    tokio::spawn(pool_event_loop(
        agent_name.clone(),
        config,
        pool_size,
        cwd,
        cmd_rx,
        completion_tx,
        completion_rx,
    ));

    tracing::info!(agent = %agent_name, pool_size, "SDK-based agent pool created");
    Ok(())
}

/// Submit a task to an agent pool. Returns a task_id.
///
/// Note: the pool was created with a fixed cwd (see `acp_pool_create`).
/// If `cwd` differs from the pool's cwd, a warning is logged but the task
/// still runs in the pool's directory. Per-task cwd would require per-task
/// sessions, which defeats the purpose of pooling.
pub async fn acp_pool_submit_task(
    agent_name: String,
    prompt: String,
    cwd: String,
) -> ErgataiResult<String> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        ErgataiError::internal(format!("Pool not found for agent: {}", agent_name))
    })?;

    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(PoolCommand::SubmitTask {
            prompt,
            cwd,
            reply_tx,
        })
        .map_err(|_| ErgataiError::internal("Pool event loop is dead".to_string()))?;

    drop(pools);

    match reply_rx.await {
        Ok(Ok(task_id)) => Ok(task_id),
        Ok(Err(e)) => Err(ErgataiError::internal(e)),
        Err(_) => Err(ErgataiError::internal("Pool event loop died".to_string())),
    }
}

/// Cancel a running or pending task.
pub async fn acp_pool_cancel_task(agent_name: String, task_id: String) -> ErgataiResult<()> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        ErgataiError::internal(format!("Pool not found for agent: {}", agent_name))
    })?;

    handle
        .cmd_tx
        .send(PoolCommand::CancelTask {
            task_id: task_id.clone(),
        })
        .map_err(|_| ErgataiError::internal("Pool event loop is dead".to_string()))?;

    Ok(())
}

/// Get pool status.
pub async fn acp_pool_status(agent_name: String) -> ErgataiResult<NapiPoolStatus> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        ErgataiError::internal(format!("Pool not found for agent: {}", agent_name))
    })?;

    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(PoolCommand::GetStatus { reply_tx })
        .map_err(|_| ErgataiError::internal("Pool event loop is dead".to_string()))?;

    drop(pools);

    reply_rx
        .await
        .map_err(|_| ErgataiError::internal("Pool event loop died".to_string()))
}

/// Shutdown an agent pool.
pub async fn acp_pool_shutdown(agent_name: String) -> ErgataiResult<()> {
    let mut pools = pool_manager().pools.write().await;
    if let Some(handle) = pools.remove(&agent_name) {
        let _ = handle.cmd_tx.send(PoolCommand::Shutdown);
        tracing::info!(agent = %agent_name, "Agent pool shutdown requested");
    }
    Ok(())
}

/// Shutdown all agent pools.
///
/// Best-effort: logs but does not abort on individual failures. Called during
/// graceful shutdown to stop all pool event loops and their SDK sessions.
pub async fn acp_pool_shutdown_all() {
    let agent_names: Vec<String> = {
        let pools = pool_manager().pools.read().await;
        pools.keys().cloned().collect()
    };

    if agent_names.is_empty() {
        return;
    }

    tracing::info!(
        count = agent_names.len(),
        "Shutting down all agent pools..."
    );
    for agent_name in agent_names {
        if let Err(e) = acp_pool_shutdown(agent_name.clone()).await {
            tracing::warn!(agent = %agent_name, error = %e, "Failed to shutdown agent pool");
        }
    }
}

/// List all agent pools.
pub async fn acp_pool_list() -> ErgataiResult<Vec<NapiPoolInfo>> {
    let pools = pool_manager().pools.read().await;
    Ok(pools
        .values()
        .map(|h| NapiPoolInfo {
            agent_name: h.agent_name.clone(),
            pool_size: h.pool_size as u32,
            status: PoolStatus::Running,
        })
        .collect())
}

// ── Pool Event Loop ──

/// Spawn a single pool agent using SDK session.
async fn spawn_pool_agent(
    config: &AgentConfig,
    agent_name: &str,
    index: usize,
    cwd: &str,
) -> ErgataiResult<PoolAgent> {
    // Use the existing session manager to create a new session
    let (session_id_tx, session_id_rx) = oneshot::channel();

    // Spawn a new SDK session — use the pool's cwd so each task runs in the
    // directory the caller requested at pool-creation time.
    crate::sdk_session::spawn_session_task(config.clone(), cwd.to_string(), session_id_tx);

    // Wait for session creation
    let session_id = session_id_rx
        .await
        .map_err(|_| ErgataiError::internal("Session creation channel died"))?
        .map_err(|e| ErgataiError::agent_init_failed_with_source("Session creation failed", e))?;

    // Get the command channel for this session
    let cmd_tx = session_manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| {
            ErgataiError::internal(format!("Session {} not found in manager", session_id))
        })?;

    tracing::info!(
        agent = %agent_name,
        index = index,
        session_id = %session_id,
        cwd = %cwd,
        "Pool agent spawned with SDK session"
    );

    Ok(PoolAgent {
        session_id,
        cmd_tx,
        busy: false,
        current_task_id: None,
    })
}

/// Guard that ensures a completion signal is sent when the task scope exits,
/// even if a panic occurs between sending the prompt and signalling completion.
/// This prevents pool agents from being permanently stuck as `busy = true`.
struct CompletionGuard {
    task_id: String,
    tx: mpsc::UnboundedSender<String>,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(self.task_id.clone());
    }
}

/// The main event loop for a pool. Manages agents and dispatches tasks.
#[allow(clippy::too_many_arguments)]
async fn pool_event_loop(
    agent_name: String,
    config: AgentConfig,
    pool_size: usize,
    cwd: String,
    mut cmd_rx: mpsc::UnboundedReceiver<PoolCommand>,
    completion_tx: mpsc::UnboundedSender<String>,
    mut completion_rx: mpsc::UnboundedReceiver<String>,
) {
    // 1. Spawn agent instances using SDK sessions.
    let mut agents: Vec<PoolAgent> = Vec::with_capacity(pool_size);
    for i in 0..pool_size {
        match spawn_pool_agent(&config, &agent_name, i, &cwd).await {
            Ok(agent) => {
                tracing::info!(agent = %agent_name, index = i, session_id = %agent.session_id, "Pool agent spawned");
                agents.push(agent);
            }
            Err(e) => {
                tracing::error!(agent = %agent_name, index = i, error = %e, "Failed to spawn pool agent");
            }
        }
    }

    if agents.is_empty() {
        tracing::error!(agent = %agent_name, "No agents spawned — pool event loop exiting");
        return;
    }

    // 2. Initialize NATS task queue (required for agent pool)
    let nats_queue: NatsTaskQueue<PendingTaskPayload> = match get_nats_connection().await {
        Some(conn) => {
            let stream_name = format!("pool_tasks_{}", agent_name.replace('-', "_"));
            let consumer_name = format!("pool_worker_{}", agent_name.replace('-', "_"));
            let subject = format!("ergatai.task.submit.pool_{}", agent_name.replace('-', "_"));

            match NatsTaskQueue::new(conn, stream_name, consumer_name, subject).await {
                Ok(queue) => {
                    tracing::info!(agent = %agent_name, "NATS task queue initialized");
                    queue
                }
                Err(e) => {
                    tracing::error!(agent = %agent_name, error = %e, "Failed to create NATS queue");
                    return;
                }
            }
        }
        None => {
            tracing::error!(agent = %agent_name, "NATS not connected — agent pool requires NATS");
            return;
        }
    };

    let mut cancelled_tasks: HashSet<String> = HashSet::new(); // Track cancelled tasks
    let evt_tx = event_tx().clone();
    let pool_cwd = cwd.clone();

    loop {
        // Try to dispatch queued tasks to idle agents.
        // Consume and dispatch tasks from NATS queue
        loop {
            let idle_idx = agents.iter().position(|a| !a.busy);
            let Some(idx) = idle_idx else {
                break; // No idle agents
            };

            // Try to consume a task from NATS
            match nats_queue.consume().await {
                Ok(Some((msg, ack))) => {
                    // Check if this task was cancelled
                    if cancelled_tasks.contains(&msg.payload.task_id) {
                        tracing::info!(task_id = %msg.payload.task_id, "Skipping cancelled task");
                        let _ = ack.ack().await;
                        cancelled_tasks.remove(&msg.payload.task_id);
                        continue;
                    }

                    let task = PendingTask {
                        task_id: msg.payload.task_id.clone(),
                        prompt: msg.payload.prompt.clone(),
                    };

                    // Dispatch the task (see below)
                    let task_id = task.task_id.clone();
                    let prompt_preview = task.prompt.chars().take(80).collect::<String>();

                    agents[idx].busy = true;
                    agents[idx].current_task_id = Some(task_id.clone());

                    let _ = evt_tx.send(SessionEvent {
                        session_id: agents[idx].session_id.clone(),
                        event_type: "task_dispatched".to_string(),
                        data: serde_json::json!({
                            "task_id": task_id,
                            "agent_index": idx,
                            "prompt_preview": prompt_preview,
                        }),
                    });

                    let session_id = agents[idx].session_id.clone();
                    let cmd_tx = agents[idx].cmd_tx.clone();
                    let task_id_clone = task_id.clone();
                    let evt_tx_clone = evt_tx.clone();
                    let completion_tx_local = completion_tx.clone();

                    tokio::spawn(async move {
                        let _completion_guard = CompletionGuard {
                            task_id: task_id_clone.clone(),
                            tx: completion_tx_local,
                        };

                        let (reply_tx, reply_rx) = oneshot::channel();

                        if let Err(e) = cmd_tx.send(SessionCommand::SendPrompt {
                            text: task.prompt.clone(),
                            reply_tx,
                        }) {
                            tracing::error!(error = %e, task_id = %task_id_clone, "Failed to send prompt to session");
                            let _ = evt_tx_clone.send(SessionEvent {
                                session_id: session_id.clone(),
                                event_type: "task_failed".to_string(),
                                data: serde_json::json!({
                                    "task_id": task_id_clone,
                                    "error": format!("Failed to send prompt: {}", e),
                                }),
                            });
                            return;
                        }

                        match tokio::time::timeout(PROMPT_MAX_DURATION, reply_rx).await {
                            Ok(Ok(Ok(()))) => {
                                tracing::info!(task_id = %task_id_clone, "Pool task completed");
                                let _ = evt_tx_clone.send(SessionEvent {
                                    session_id: session_id.clone(),
                                    event_type: "task_completed".to_string(),
                                    data: serde_json::json!({
                                        "task_id": task_id_clone,
                                    }),
                                });
                                // Ack the NATS message on success
                                let _ = ack.ack().await;
                            }
                            Ok(Ok(Err(e))) => {
                                tracing::error!(error = %e, task_id = %task_id_clone, "Pool task failed");
                                let _ = evt_tx_clone.send(SessionEvent {
                                    session_id: session_id.clone(),
                                    event_type: "task_failed".to_string(),
                                    data: serde_json::json!({
                                        "task_id": task_id_clone,
                                        "error": format!("{}", e),
                                    }),
                                });
                                // Ack on failure too (don't retry)
                                let _ = ack.ack().await;
                            }
                            Ok(Err(_)) => {
                                tracing::error!(task_id = %task_id_clone, "Reply channel died");
                                let _ = evt_tx_clone.send(SessionEvent {
                                    session_id: session_id.clone(),
                                    event_type: "task_failed".to_string(),
                                    data: serde_json::json!({
                                        "task_id": task_id_clone,
                                        "error": "Reply channel died",
                                    }),
                                });
                                let _ = ack.ack().await;
                            }
                            Err(_) => {
                                tracing::error!(task_id = %task_id_clone, "Pool task timed out");
                                let _ = evt_tx_clone.send(SessionEvent {
                                    session_id: session_id.clone(),
                                    event_type: "task_failed".to_string(),
                                    data: serde_json::json!({
                                        "task_id": task_id_clone,
                                        "error": "Task timed out",
                                    }),
                                });
                                // Nack on timeout (allow retry)
                                let _ = ack.nack().await;
                            }
                        }
                    });
                }
                Ok(None) => {
                    break; // No more tasks in queue
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to consume from NATS queue");
                    break;
                }
            }
        }

        // Wait for the next command or task-completion signal.
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PoolCommand::SubmitTask { prompt, cwd, reply_tx }) => {
                        let task_id = format!("pool-task-{}", TASK_ID_COUNTER.fetch_add(1, Ordering::Relaxed));
                        if cwd != pool_cwd {
                            tracing::warn!(
                                task_id = %task_id,
                                agent = %agent_name,
                                requested_cwd = %cwd,
                                pool_cwd = %pool_cwd,
                                "Task submitted with cwd differing from pool cwd — \
                                 using pool cwd (per-task cwd not supported)"
                            );
                        }
                        tracing::info!(task_id = %task_id, agent = %agent_name, "Task submitted to pool");

                        // Submit to NATS queue first
                        let payload = PendingTaskPayload {
                            task_id: task_id.clone(),
                            prompt: prompt.clone(),
                            agent_name: agent_name.clone(),
                        };
                        match nats_queue.submit(agent_name.clone(), payload).await {
                            Ok(_) => {
                                let _ = reply_tx.send(Ok(task_id));
                            }
                            Err(e) => {
                                tracing::error!(error = %e, task_id = %task_id, "Failed to submit to NATS queue");
                                let _ = reply_tx.send(Err(format!("Failed to submit task: {}", e)));
                            }
                        }
                    }
                    Some(PoolCommand::CancelTask { task_id }) => {
                        tracing::info!(task_id = %task_id, agent = %agent_name, "Task cancel requested");
                        // Add to cancelled set - will be skipped when consumed from NATS
                        cancelled_tasks.insert(task_id.clone());
                        tracing::info!(task_id = %task_id, "Task added to cancelled set");
                    }
                    Some(PoolCommand::GetStatus { reply_tx }) => {
                        let (idle, busy) = agents.iter().fold((0, 0), |(i, b), a| {
                            if a.busy { (i, b + 1) } else { (i + 1, b) }
                        });

                        // Get pending count from NATS queue
                        let pending = nats_queue.pending_count().await.unwrap_or(0) as u32;

                        let _ = reply_tx.send(NapiPoolStatus {
                            agent_name: agent_name.clone(),
                            pool_size: agents.len() as u32,
                            idle_agents: idle as u32,
                            busy_agents: busy as u32,
                            pending_tasks: pending,
                            in_flight_tasks: busy as u32,
                        });
                    }
                    Some(PoolCommand::Shutdown) | None => {
                        tracing::info!(agent = %agent_name, "Pool event loop shutting down");
                        // Close all sessions.
                        for agent in &agents {
                            let _ = agent.cmd_tx.send(SessionCommand::Close);
                        }
                        break;
                    }
                }
            }
            completed_task_id = completion_rx.recv() => {
                let Some(task_id) = completed_task_id else {
                    // All completion senders dropped — should not happen while
                    // agents exist, but treat as shutdown to avoid spin-looping.
                    tracing::warn!(agent = %agent_name, "Completion channel closed, shutting down pool");
                    for agent in &agents {
                        let _ = agent.cmd_tx.send(SessionCommand::Close);
                    }
                    break;
                };

                // Find the agent that was running this task and free it.
                let freed_idx = agents.iter().position(|a| a.current_task_id.as_deref() == Some(&task_id));
                if let Some(idx) = freed_idx {
                    agents[idx].busy = false;
                    agents[idx].current_task_id = None;
                    tracing::info!(
                        agent = %agent_name,
                        index = idx,
                        task_id = %task_id,
                        "Agent freed after task completion"
                    );
                } else {
                    tracing::warn!(
                        agent = %agent_name,
                        task_id = %task_id,
                        "Completion received for unknown task — ignoring"
                    );
                }
            }
        }
    }
}
