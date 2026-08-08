//! SDK-based Agent Pool Manager — manages multiple SDK session instances for concurrent task execution.
//!
//! Maintains high-level abstractions: agent pool, task queue, load balancing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use napi_derive::napi;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::acp::manager::{SessionCommand, SessionEvent, event_tx, manager as session_manager};
use crate::agent::config::AgentConfig;
use crate::error::{ErgataiError, ErgataiResult};

/// Global task ID counter.
static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Pool-level timeouts.
const PROMPT_MAX_DURATION: Duration = Duration::from_secs(7200); // 2 hours

// ── NAPI Types ──

/// Pool status returned to the frontend.
#[napi(object)]
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
#[napi(string_enum)]
#[derive(Debug, Serialize)]
pub enum PoolStatus {
    Running,
    Stopped,
}

/// Pool info for listing.
#[napi(object)]
#[derive(Debug, Clone, Serialize)]
pub struct NapiPoolInfo {
    pub agent_name: String,
    pub pool_size: u32,
    pub status: PoolStatus,
}

/// Task info returned to the frontend.
#[napi(object)]
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

// ── NAPI Functions ──

/// Create an agent pool with N concurrent SDK session instances.
#[napi]
pub async fn acp_pool_create(
    agent_name: String,
    pool_size: u32,
    cwd: String,
) -> napi::Result<()> {
    let config = crate::agent::config::get_agent_config(&agent_name)
        .map_err(|e| napi::Error::from_reason(format!("Failed to load agent config: {}", e)))?;

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
            return Err(napi::Error::from_reason(format!(
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
#[napi]
pub async fn acp_pool_submit_task(
    agent_name: String,
    prompt: String,
    cwd: String,
) -> napi::Result<String> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        napi::Error::from_reason(format!("Pool not found for agent: {}", agent_name))
    })?;

    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(PoolCommand::SubmitTask {
            prompt,
            cwd,
            reply_tx,
        })
        .map_err(|_| napi::Error::from_reason("Pool event loop is dead"))?;

    drop(pools);

    match reply_rx.await {
        Ok(Ok(task_id)) => Ok(task_id),
        Ok(Err(e)) => Err(napi::Error::from_reason(e)),
        Err(_) => Err(napi::Error::from_reason("Pool event loop died")),
    }
}

/// Cancel a running or pending task.
#[napi]
pub async fn acp_pool_cancel_task(agent_name: String, task_id: String) -> napi::Result<()> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        napi::Error::from_reason(format!("Pool not found for agent: {}", agent_name))
    })?;

    handle
        .cmd_tx
        .send(PoolCommand::CancelTask {
            task_id: task_id.clone(),
        })
        .map_err(|_| napi::Error::from_reason("Pool event loop is dead"))?;

    Ok(())
}

/// Get pool status.
#[napi]
pub async fn acp_pool_status(agent_name: String) -> napi::Result<NapiPoolStatus> {
    let pools = pool_manager().pools.read().await;
    let handle = pools.get(&agent_name).ok_or_else(|| {
        napi::Error::from_reason(format!("Pool not found for agent: {}", agent_name))
    })?;

    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(PoolCommand::GetStatus { reply_tx })
        .map_err(|_| napi::Error::from_reason("Pool event loop is dead"))?;

    drop(pools);

    reply_rx
        .await
        .map_err(|_| napi::Error::from_reason("Pool event loop died"))
}

/// Shutdown an agent pool.
#[napi]
pub async fn acp_pool_shutdown(agent_name: String) -> napi::Result<()> {
    let mut pools = pool_manager().pools.write().await;
    if let Some(handle) = pools.remove(&agent_name) {
        let _ = handle.cmd_tx.send(PoolCommand::Shutdown);
        tracing::info!(agent = %agent_name, "Agent pool shutdown requested");
    }
    Ok(())
}

/// List all agent pools.
#[napi]
pub async fn acp_pool_list() -> napi::Result<Vec<NapiPoolInfo>> {
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
    crate::acp::sdk_session::spawn_session_task(
        config.clone(),
        cwd.to_string(),
        session_id_tx,
    );

    // Wait for session creation
    let session_id = session_id_rx
        .await
        .map_err(|_| ErgataiError::internal("Session creation channel died"))?
        .map_err(|e| ErgataiError::agent_init_failed_with_source("Session creation failed", e))?;

    // Get the command channel for this session
    let cmd_tx = session_manager()
        .get_cmd_tx(&session_id)
        .await
        .ok_or_else(|| ErgataiError::internal(format!("Session {} not found in manager", session_id)))?;

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

    // 2. Task queue and event loop.
    let mut task_queue: VecDeque<PendingTask> = VecDeque::new();
    let evt_tx = event_tx().clone();
    let pool_cwd = cwd.clone();

    loop {
        // Try to dispatch queued tasks to idle agents.
        while let Some(task) = task_queue.pop_front() {
            let idle_idx = agents.iter().position(|a| !a.busy);
            let Some(idx) = idle_idx else {
                // No idle agents — put the task back at the front and stop.
                task_queue.push_front(task);
                break;
            };

            let task_id = task.task_id.clone();
            let prompt_preview = task.prompt.chars().take(80).collect::<String>();

            // Mark agent as busy.
            agents[idx].busy = true;
            agents[idx].current_task_id = Some(task_id.clone());

            // Emit task_dispatched event.
            let _ = evt_tx.send(SessionEvent {
                session_id: agents[idx].session_id.clone(),
                event_type: "task_dispatched".to_string(),
                data: serde_json::json!({
                    "task_id": task_id,
                    "agent_index": idx,
                    "prompt_preview": prompt_preview,
                }),
            });

            // Execute the task via SDK session.
            let session_id = agents[idx].session_id.clone();
            let cmd_tx = agents[idx].cmd_tx.clone();
            let task_id_clone = task_id.clone();
            let evt_tx_clone = evt_tx.clone();
            let completion_tx_local = completion_tx.clone();

            tokio::spawn(async move {
                // CompletionGuard ensures the completion signal is sent even on panic,
                // preventing the agent from being permanently stuck as `busy = true`.
                let _completion_guard = CompletionGuard {
                    task_id: task_id_clone.clone(),
                    tx: completion_tx_local,
                };

                let (reply_tx, reply_rx) = oneshot::channel();

                // Send prompt to SDK session
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
                    return; // _completion_guard dropped here, sends completion signal
                }

                // Wait for completion with timeout
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
                    }
                }

                // _completion_guard dropped here, sends completion signal
            });

            // Agent remains busy until the spawned task above sends a completion
            // message back via completion_tx — handled in the select! below.
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
                        let _ = reply_tx.send(Ok(task_id.clone()));
                        task_queue.push_back(PendingTask {
                            task_id,
                            prompt,
                        });
                    }
                    Some(PoolCommand::CancelTask { task_id }) => {
                        tracing::info!(task_id = %task_id, agent = %agent_name, "Task cancel requested");
                        // Remove from queue if pending.
                        let was_pending = task_queue.iter().any(|t| t.task_id == task_id);
                        task_queue.retain(|t| t.task_id != task_id);
                        if !was_pending {
                            // Running-task cancel requires SDK session support; for now
                            // we surface the limitation rather than silently no-op.
                            tracing::warn!(
                                task_id = %task_id,
                                "Cancel requested for a running task — not yet supported by SDK session; \
                                 task will continue until completion"
                            );
                        }
                    }
                    Some(PoolCommand::GetStatus { reply_tx }) => {
                        let idle = agents.iter().filter(|a| !a.busy).count();
                        let busy = agents.iter().filter(|a| a.busy).count();
                        let _ = reply_tx.send(NapiPoolStatus {
                            agent_name: agent_name.clone(),
                            pool_size: agents.len() as u32,
                            idle_agents: idle as u32,
                            busy_agents: busy as u32,
                            pending_tasks: task_queue.len() as u32,
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
