use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::{mpsc, RwLock};

use serde::Serialize;

/// Session 命令（内部使用，不导出 NAPI）
pub enum SessionCommand {
    SendPrompt {
        text: String,
        reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    SetMode {
        mode_id: String,
        reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    SetConfigOption {
        config_id: String,
        value_id: String,
        reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    /// Steering: inject a mid-turn message without cancelling the current prompt.
    Steer {
        text: String,
        reply_tx: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    },
    /// 权限请求响应（从前端发回）
    PermissionResponse {
        request_id: String,
        option_id: Option<String>,
    },
    Close,
}

/// 权限请求（从 agent 发起到前端）
#[derive(serde::Serialize)]
pub struct NapiPermissionRequest {
    pub session_id: String,
    pub request_id: String,
    pub tool_name: Option<String>,
    pub options: Vec<NapiPermissionOption>,
}

#[derive(serde::Serialize)]
pub struct NapiPermissionOption {
    pub option_id: String,
    pub label: String,
}

/// Session 事件（推送到前端）
#[derive(Debug, Clone, Serialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub event_type: String,
    pub data: serde_json::Value,
}

/// NAPI 导出的 Session 事件（data 序列化为 JSON 字符串）
pub struct NapiSessionEvent {
    pub session_id: String,
    pub event_type: String,
    pub data: String,
}

impl From<SessionEvent> for NapiSessionEvent {
    fn from(e: SessionEvent) -> Self {
        let session_id_for_log = e.session_id.clone();
        Self {
            session_id: e.session_id,
            event_type: e.event_type,
            data: serde_json::to_string(&e.data).unwrap_or_else(|err| {
                tracing::warn!(
                    "Failed to serialize session event data for session '{}': {}",
                    session_id_for_log,
                    err
                );
                "null".to_string()
            }),
        }
    }
}

/// Session kind — distinguishes DAG orchestration sessions from user chat sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum SessionKind {
    /// Interactive user chat session (via NAPI / UI) — default
    #[default]
    Chat,
    /// Unattended DAG orchestration session (auto-approves permissions)
    Dag,
}

/// Session 句柄（持有向 session task 发送命令的 channel）
pub struct SessionHandle {
    pub session_id: String,
    pub agent_name: String,
    pub cwd: String,
    pub cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    pub kind: SessionKind,
    /// Abort handle for the session task — allows `close_all` to cancel
    /// orphaned tasks on timeout instead of just removing them from the map.
    pub abort_handle: Option<tokio::task::AbortHandle>,
}

/// 全局会话管理器
pub struct SessionManager {
    sessions: RwLock<HashMap<String, SessionHandle>>,
    /// Watch channel for session count changes — allows close_all to wait efficiently
    /// instead of polling with sleep.
    session_count_watch: (
        tokio::sync::watch::Sender<usize>,
        tokio::sync::watch::Receiver<usize>,
    ),
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::watch::channel(0);
        Self {
            sessions: RwLock::new(HashMap::new()),
            session_count_watch: (tx, rx),
        }
    }

    pub async fn register(&self, handle: SessionHandle) {
        // Compute count while holding the write lock to avoid TOCTOU race
        let count = {
            let mut sessions = self.sessions.write().await;
            sessions.insert(handle.session_id.clone(), handle);
            sessions.len()
        };
        let _ = self.session_count_watch.0.send(count);
    }

    pub async fn unregister(&self, session_id: &str) {
        // Compute count while holding the write lock to avoid TOCTOU race
        let count = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_id);
            sessions.len()
        };
        let _ = self.session_count_watch.0.send(count);
    }

    pub async fn get_cmd_tx(
        &self,
        session_id: &str,
    ) -> Option<mpsc::UnboundedSender<SessionCommand>> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|h| h.cmd_tx.clone())
    }

    pub async fn list_sessions(&self) -> Vec<NapiSessionInfo> {
        self.sessions
            .read()
            .await
            .values()
            .map(|h| NapiSessionInfo {
                session_id: h.session_id.clone(),
                agent_name: h.agent_name.clone(),
                cwd: h.cwd.clone(),
                status: "active".to_string(),
                title: None,
                updated_at: None,
            })
            .collect()
    }

    /// 关闭所有会话（优雅关闭）
    pub async fn close_all(&self) {
        // Send Close command to all active sessions
        let session_ids: Vec<String> = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect()
        };

        for session_id in &session_ids {
            if let Some(cmd_tx) = self.get_cmd_tx(session_id).await {
                let _ = cmd_tx.send(SessionCommand::Close);
            }
        }

        // Wait for sessions to actually close using the watch channel
        // (much more efficient than polling with sleep)
        let timeout_duration = std::time::Duration::from_secs(5);
        let mut rx = self.session_count_watch.1.clone();

        // Wait for count to reach 0 or timeout
        match tokio::time::timeout(timeout_duration, async {
            loop {
                if *rx.borrow_and_update() == 0 {
                    break Ok::<(), tokio::sync::watch::error::RecvError>(());
                }
                if rx.changed().await.is_err() {
                    // Sender dropped, treat as shutdown
                    break Ok(());
                }
            }
        })
        .await
        {
            Ok(Ok(())) => {
                tracing::info!("All {} sessions closed gracefully", session_ids.len());
            }
            Ok(Err(_)) => {
                // Should not happen, but handle gracefully
                tracing::warn!("Watch channel error while waiting for sessions to close");
            }
            Err(_) => {
                // Timeout — abort remaining session tasks to prevent orphan leaks,
                // then force-unregister from the map in a single write-lock scope.
                let remaining_ids: Vec<String> = {
                    let sessions = self.sessions.read().await;
                    tracing::warn!(
                        "Timeout waiting for sessions to close, {} still active — aborting tasks",
                        sessions.len()
                    );
                    // First: abort all remaining tasks (cancels inner tokio::spawn)
                    for handle in sessions.values() {
                        if let Some(ref abort_handle) = handle.abort_handle {
                            abort_handle.abort();
                        }
                    }
                    sessions.keys().cloned().collect()
                };
                // Second: batch-remove in a single write-lock scope to keep count consistent
                let final_count = {
                    let mut sessions = self.sessions.write().await;
                    for id in &remaining_ids {
                        sessions.remove(id);
                    }
                    sessions.len()
                };
                let _ = self.session_count_watch.0.send(final_count);
            }
        }
    }
    /// 关闭所有 Chat 类型会话（不影响 DAG 会话）
    /// UI 关闭面板/切换项目时调用，避免误关 DAG 编排会话。
    ///
    /// Note: This method sends Close commands and waits up to 5 seconds for
    /// sessions to close. If DAG sessions are concurrently added/removed, the
    /// target count may be slightly off — this is acceptable for shutdown use cases.
    pub async fn close_chat_sessions(&self) {
        let (chat_ids, dag_count): (Vec<String>, usize) = {
            let sessions = self.sessions.read().await;
            let mut chat_ids = Vec::with_capacity(sessions.len());
            let mut dag_count = 0;
            for h in sessions.values() {
                if h.kind == SessionKind::Chat {
                    chat_ids.push(h.session_id.clone());
                } else {
                    dag_count += 1;
                }
            }
            (chat_ids, dag_count)
        };

        for session_id in &chat_ids {
            if let Some(cmd_tx) = self.get_cmd_tx(session_id).await {
                let _ = cmd_tx.send(SessionCommand::Close);
            }
        }

        // Wait for chat sessions to close (reuse watch channel)
        let timeout_duration = std::time::Duration::from_secs(5);
        let mut rx = self.session_count_watch.1.clone();

        let _ = tokio::time::timeout(timeout_duration, async {
            loop {
                // Target: only DAG sessions remain
                if *rx.borrow_and_update() <= dag_count {
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }

    /// Close only sessions of a specific kind and wait for them to close.
    pub async fn close_by_kind(&self, kind: SessionKind) {
        let (ids, other_count): (Vec<String>, usize) = {
            let sessions = self.sessions.read().await;
            let mut ids = Vec::with_capacity(sessions.len());
            let mut other_count = 0;
            for h in sessions.values() {
                if h.kind == kind {
                    ids.push(h.session_id.clone());
                } else {
                    other_count += 1;
                }
            }
            (ids, other_count)
        };

        for session_id in &ids {
            if let Some(cmd_tx) = self.get_cmd_tx(session_id).await {
                let _ = cmd_tx.send(SessionCommand::Close);
            }
        }

        // Wait for targeted sessions to close
        let timeout_duration = std::time::Duration::from_secs(5);
        let mut rx = self.session_count_watch.1.clone();

        let _ = tokio::time::timeout(timeout_duration, async {
            loop {
                // Target: only sessions of other kinds remain
                if *rx.borrow_and_update() <= other_count {
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NapiSessionInfo {
    pub session_id: String,
    pub agent_name: String,
    pub cwd: String,
    pub status: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

// ── 全局状态 ──

struct GlobalState {
    manager: SessionManager,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    event_rx: Mutex<mpsc::UnboundedReceiver<SessionEvent>>,
}

static STATE: OnceLock<GlobalState> = OnceLock::new();

fn state() -> &'static GlobalState {
    STATE.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        GlobalState {
            manager: SessionManager::new(),
            event_tx: tx,
            event_rx: Mutex::new(rx),
        }
    })
}

pub fn manager() -> &'static SessionManager {
    &state().manager
}

pub fn event_tx() -> &'static mpsc::UnboundedSender<SessionEvent> {
    &state().event_tx
}

/// 取出所有待处理事件（TS 侧轮询调用）
pub fn poll_events() -> Vec<NapiSessionEvent> {
    let mut rx = match state().event_rx.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(
                "event_rx mutex poisoned (likely a panic while holding the lock). \
                 Recovering — any in-flight events not yet drained may be lost."
            );
            poisoned.into_inner()
        }
    };
    let mut events = vec![];
    while let Ok(event) = rx.try_recv() {
        events.push(event.into());
    }
    events
}
