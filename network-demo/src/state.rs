use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Agent 运行状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum AgentState {
    /// 正在建立 ACP 连接
    Connecting,
    /// 空闲，可以接受任务
    Idle,
    /// 正在处理任务
    Busy {
        task_id: String,
        #[serde(skip)]
        since: Option<Instant>,
    },
    /// 出错
    Error {
        reason: String,
        retryable: bool,
    },
    /// 已断开（进程退出或崩溃）
    Dead,
}

impl AgentState {
    /// 是否可以接受新任务
    pub fn can_accept_task(&self) -> bool {
        matches!(self, AgentState::Idle)
    }

    /// 状态名（用于 NAPI 导出）
    pub fn name(&self) -> &'static str {
        match self {
            AgentState::Connecting => "connecting",
            AgentState::Idle => "idle",
            AgentState::Busy { .. } => "busy",
            AgentState::Error { .. } => "error",
            AgentState::Dead => "dead",
        }
    }

    /// 是否存活（非 Dead 且非 Error）
    pub fn is_alive(&self) -> bool {
        !matches!(self, AgentState::Dead)
    }
}

impl Default for AgentState {
    fn default() -> Self {
        AgentState::Connecting
    }
}
