use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

/// Agent 唯一标识（即 agent 配置名）
pub type AgentId = String;

/// Agent 间通信的消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// 任务分发：A 让 B 做某事
    Task {
        from: AgentId,
        task_id: String,
        payload: serde_json::Value,
    },
    /// 任务结果：B 完成任务后回报
    Result {
        from: AgentId,
        task_id: String,
        payload: serde_json::Value,
    },
    /// 频道事件：广播到某个 channel
    Event {
        from: AgentId,
        channel: String,
        data: serde_json::Value,
    },
}

/// 接收到的消息（带 ACK 语义，为 JetStream 预留）
#[allow(dead_code)]
pub struct IncomingMessage {
    pub msg: Message,
    /// InMemory 实现直接 drop；JetStream 实现发送 ACK 确认处理完成
    pub ack_tx: oneshot::Sender<()>,
}

impl IncomingMessage {
    pub fn new(msg: Message) -> (Self, oneshot::Receiver<()>) {
        let (ack_tx, ack_rx) = oneshot::channel();
        (Self { msg, ack_tx }, ack_rx)
    }
}

/// 传输层健康状态
#[derive(Debug, Clone, Serialize)]
pub struct HealthStatus {
    /// 是否健康
    pub healthy: bool,
    /// 在线 agent 数量
    pub agent_count: usize,
    /// 在线 agent 列表
    pub agents: Vec<AgentId>,
    /// 活跃频道数量
    pub channel_count: usize,
    /// 活跃频道列表
    pub channels: Vec<String>,
    /// 额外信息（如连接状态、延迟等）
    pub details: HashMap<String, serde_json::Value>,
}

/// 通信层核心接口。
///
/// 开源版用 `InMemoryTransport`（进程内 channel），
/// Pro 版用 `NatsTransport`（NATS/JetStream，跨进程/跨设备）。
///
/// 上层协作逻辑（Pipeline、Dispatcher 等）只依赖此 trait，不关心底层实现。
#[async_trait::async_trait]
pub trait AgentTransport: Send + Sync + 'static {
    /// 点对点发送消息给目标 agent
    async fn send(&self, target: &AgentId, msg: Message) -> Result<()>;

    /// 广播消息到指定频道，所有订阅者都会收到
    async fn publish(&self, channel: &str, msg: Message) -> Result<()>;

    /// 订阅 agent 的 mailbox，返回接收端
    async fn subscribe(&self, agent_id: &AgentId) -> Result<mpsc::UnboundedReceiver<IncomingMessage>>;

    /// 取消订阅，移除 mailbox
    async fn unsubscribe(&self, agent_id: &AgentId) -> Result<()>;

    /// 检查 agent 是否在线（有活跃的 mailbox）
    fn is_alive(&self, agent_id: &AgentId) -> bool;

    /// 加入频道
    ///
    /// agent 将收到该频道的所有广播消息
    async fn join_channel(&self, agent_id: &AgentId, channel: &str) -> Result<()>;

    /// 离开频道
    async fn leave_channel(&self, agent_id: &AgentId, channel: &str) -> Result<()>;

    /// 优雅关闭传输层
    ///
    /// 停止所有后台任务，清理资源
    async fn shutdown(&self);

    /// 获取传输层健康状态
    fn health_check(&self) -> HealthStatus;
}
