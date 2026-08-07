use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Result, bail};
use tokio::sync::mpsc;

use super::in_memory::InMemoryTransport;
use super::message::AgentFriendlyMessage;
use super::parser::MessageParser;
use super::state::AgentState;
use super::task::TaskManager;
use super::transport::{AgentId, AgentTransport, IncomingMessage, Message};

/// Agent 网络 — 管理 agent 间通信和状态。
///
/// 上层协作逻辑（Pipeline、Dispatcher 等）通过此类交互。
/// 底层传输通过 `AgentTransport` trait 抽象，可替换实现。
pub struct AgentNetwork {
    transport: Arc<dyn AgentTransport>,
    states: Arc<RwLock<HashMap<AgentId, AgentState>>>,
    task_manager: Arc<TaskManager>,
}

impl AgentNetwork {
    pub fn new(transport: Arc<dyn AgentTransport>) -> Self {
        Self {
            transport,
            states: Arc::new(RwLock::new(HashMap::new())),
            task_manager: Arc::new(TaskManager::new()),
        }
    }

    /// 注册 agent 到网络，创建 mailbox
    pub async fn register_agent(&self, id: AgentId) -> Result<mpsc::UnboundedReceiver<IncomingMessage>> {
        let rx = self.transport.subscribe(&id).await?;
        self.states
            .write()
            .unwrap()
            .insert(id.clone(), AgentState::Idle);
        Ok(rx)
    }

    /// 注销 agent
    pub async fn unregister_agent(&self, id: &AgentId) -> Result<()> {
        self.transport.unsubscribe(id).await?;
        self.states.write().unwrap().remove(id);
        Ok(())
    }

    /// 发送任务给目标 agent
    pub async fn send_task(
        &self,
        from: &AgentId,
        target: &AgentId,
        task_id: String,
        payload: serde_json::Value,
    ) -> Result<()> {
        if !self.transport.is_alive(target) {
            bail!("Target agent not alive: {}", target);
        }
        self.transport
            .send(
                target,
                Message::Task {
                    from: from.clone(),
                    task_id,
                    payload,
                },
            )
            .await
    }

    /// 发送任务结果
    pub async fn send_result(
        &self,
        from: &AgentId,
        target: &AgentId,
        task_id: String,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.transport
            .send(
                target,
                Message::Result {
                    from: from.clone(),
                    task_id,
                    payload,
                },
            )
            .await
    }

    /// 广播到频道
    pub async fn broadcast(
        &self,
        from: &AgentId,
        channel: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        self.transport
            .publish(
                channel,
                Message::Event {
                    from: from.clone(),
                    channel: channel.to_string(),
                    data,
                },
            )
            .await
    }

    /// 发送 AI 友好消息（自动转换为标准格式）
    pub async fn send_friendly(
        &self,
        from: &AgentId,
        target: &AgentId,
        msg: AgentFriendlyMessage,
    ) -> Result<()> {
        if !self.transport.is_alive(target) {
            bail!("Target agent not alive: {}", target);
        }
        let standard = MessageParser::parse(msg, from.clone());
        let payload = serde_json::to_value(&standard)?;
        self.transport
            .send(
                target,
                Message::Event {
                    from: from.clone(),
                    channel: format!("dm:{}", target),
                    data: payload,
                },
            )
            .await
    }

    /// 广播 AI 友好消息（自动转换为标准格式）
    pub async fn broadcast_friendly(
        &self,
        from: &AgentId,
        channel: &str,
        msg: AgentFriendlyMessage,
    ) -> Result<()> {
        let standard = MessageParser::parse(msg, from.clone());
        let payload = serde_json::to_value(&standard)?;
        self.transport
            .publish(
                channel,
                Message::Event {
                    from: from.clone(),
                    channel: channel.to_string(),
                    data: payload,
                },
            )
            .await
    }

    /// 获取任务管理器
    pub fn task_manager(&self) -> &Arc<TaskManager> {
        &self.task_manager
    }

    /// 获取 agent 状态
    pub fn get_state(&self, id: &AgentId) -> AgentState {
        self.states
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or(AgentState::Dead)
    }

    /// 更新 agent 状态
    #[allow(dead_code)]
    pub fn set_state(&self, id: &AgentId, state: AgentState) {
        self.states.write().unwrap().insert(id.clone(), state);
    }

    /// 列出所有 agent 及状态
    pub fn list_agents(&self) -> Vec<NapiAgentInfo> {
        self.states
            .read()
            .unwrap()
            .iter()
            .map(|(id, state)| NapiAgentInfo {
                agent_id: id.clone(),
                state: state.name().to_string(),
                alive: state.is_alive(),
                can_accept_task: state.can_accept_task(),
            })
            .collect()
    }

    /// 检查 agent 是否在线
    #[allow(dead_code)]
    pub fn is_alive(&self, id: &AgentId) -> bool {
        self.transport.is_alive(id)
    }

    /// 获取底层 transport（用于高级操作如 join_channel）
    #[allow(dead_code)]
    pub fn transport(&self) -> &Arc<dyn AgentTransport> {
        &self.transport
    }
}

#[derive(Debug, Clone)]
pub struct NapiAgentInfo {
    pub agent_id: String,
    pub state: String,
    pub alive: bool,
    pub can_accept_task: bool,
}

// ── 全局单例 ──

use std::sync::OnceLock;

static NETWORK: OnceLock<AgentNetwork> = OnceLock::new();

/// 获取全局 AgentNetwork（默认使用 InMemoryTransport）
pub fn get_network() -> &'static AgentNetwork {
    NETWORK.get_or_init(|| AgentNetwork::new(Arc::new(InMemoryTransport::new())))
}

/// 用指定 transport 初始化全局 AgentNetwork。
/// 只能在应用启动时调用一次，之后调用会返回错误。
pub fn init_network(transport: Arc<dyn AgentTransport>) -> Result<()> {
    NETWORK
        .set(AgentNetwork::new(transport))
        .map_err(|_| anyhow::anyhow!("AgentNetwork already initialized"))
}
