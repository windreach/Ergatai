use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::sync::mpsc;

use super::transport::{AgentId, AgentTransport, HealthStatus, IncomingMessage, Message};

/// 进程内通信实现（开源版）。
///
/// 基于 tokio channel，零延迟，无持久化。
/// Agent 必须在同一进程内。Pro 版替换为 NATS/JetStream 即可跨进程/跨设备。
///
/// 特性：
/// - 与 NatsTransport API 完全一致
/// - 支持频道订阅（join_channel / leave_channel）
/// - 健康检查接口
/// - 优雅关闭
pub struct InMemoryTransport {
    /// agent_id → mailbox sender
    mailboxes: Arc<RwLock<HashMap<AgentId, mpsc::UnboundedSender<IncomingMessage>>>>,
    /// channel_name → 订阅者 agent_id 集合
    channels: Arc<RwLock<HashMap<String, HashSet<AgentId>>>>,
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self {
            mailboxes: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentTransport for InMemoryTransport {
    async fn send(&self, target: &AgentId, msg: Message) -> Result<()> {
        let mailboxes = self.mailboxes.read().unwrap();
        let tx = mailboxes
            .get(target)
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", target))?;
        let (incoming, _ack_rx) = IncomingMessage::new(msg);
        tx.send(incoming)
            .map_err(|_| anyhow::anyhow!("Agent mailbox closed: {}", target))
    }

    async fn publish(&self, channel: &str, msg: Message) -> Result<()> {
        let channels = self.channels.read().unwrap();
        let subscribers = match channels.get(channel) {
            Some(s) => s.clone(),
            None => return Ok(()), // 没人订阅，静默忽略
        };
        drop(channels); // 释放读锁

        let mailboxes = self.mailboxes.read().unwrap();
        for agent_id in &subscribers {
            if let Some(tx) = mailboxes.get(agent_id) {
                let (incoming, _ack_rx) = IncomingMessage::new(msg.clone());
                let _ = tx.send(incoming); // 某个 agent 接收失败不影响其他
            }
        }
        Ok(())
    }

    async fn subscribe(
        &self,
        agent_id: &AgentId,
    ) -> Result<mpsc::UnboundedReceiver<IncomingMessage>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.mailboxes.write().unwrap().insert(agent_id.clone(), tx);
        Ok(rx)
    }

    async fn unsubscribe(&self, agent_id: &AgentId) -> Result<()> {
        // 移除 mailbox
        self.mailboxes.write().unwrap().remove(agent_id);

        // 从所有频道中移除
        let mut channels = self.channels.write().unwrap();
        for subscribers in channels.values_mut() {
            subscribers.remove(agent_id);
        }

        // 清理空频道
        channels.retain(|_, subs| !subs.is_empty());

        Ok(())
    }

    fn is_alive(&self, agent_id: &AgentId) -> bool {
        self.mailboxes.read().unwrap().contains_key(agent_id)
    }

    async fn join_channel(&self, agent_id: &AgentId, channel: &str) -> Result<()> {
        // 检查 agent 是否已订阅
        if !self.is_alive(agent_id) {
            return Err(anyhow::anyhow!("Agent not subscribed: {}", agent_id));
        }

        let mut channels = self.channels.write().unwrap();
        channels
            .entry(channel.to_string())
            .or_default()
            .insert(agent_id.clone());

        Ok(())
    }

    async fn leave_channel(&self, agent_id: &AgentId, channel: &str) -> Result<()> {
        let mut channels = self.channels.write().unwrap();
        if let Some(subscribers) = channels.get_mut(channel) {
            subscribers.remove(agent_id);

            // 清理空频道
            if subscribers.is_empty() {
                channels.remove(channel);
            }
        }

        Ok(())
    }

    async fn shutdown(&self) {
        // 清空所有 mailbox
        let mut mailboxes = self.mailboxes.write().unwrap();
        mailboxes.clear();

        // 清空所有频道订阅
        let mut channels = self.channels.write().unwrap();
        channels.clear();

        // InMemory 没有后台任务，直接完成
    }

    fn health_check(&self) -> HealthStatus {
        let mailboxes = self.mailboxes.read().unwrap();
        let channels = self.channels.read().unwrap();

        let agents: Vec<AgentId> = mailboxes.keys().cloned().collect();
        let agent_count = agents.len();

        let channel_list: Vec<String> = channels.keys().cloned().collect();
        let channel_count = channel_list.len();

        HealthStatus {
            healthy: true, // InMemory 总是健康的
            agent_count,
            agents,
            channel_count,
            channels: channel_list,
            details: {
                let mut map = HashMap::new();
                map.insert("type".to_string(), serde_json::json!("in_memory"));
                map.insert("persistent".to_string(), serde_json::json!(false));
                map
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_point_to_point() {
        let transport = InMemoryTransport::new();

        // 注册两个 agent
        let mut rx_a = transport.subscribe(&"agent_a".into()).await.unwrap();
        let mut rx_b = transport.subscribe(&"agent_b".into()).await.unwrap();

        // A 发消息给 B
        transport
            .send(
                &"agent_b".into(),
                Message::Task {
                    from: "agent_a".into(),
                    task_id: "t1".into(),
                    payload: json!({"action": "review"}),
                },
            )
            .await
            .unwrap();

        // B 收到消息
        let incoming = rx_b.recv().await.unwrap();
        assert!(matches!(incoming.msg, Message::Task { .. }));
        if let Message::Task { from, task_id, .. } = incoming.msg {
            assert_eq!(from, "agent_a");
            assert_eq!(task_id, "t1");
        }

        // A 没收到
        assert!(rx_a.try_recv().is_err());

        // 发给不存在的 agent 应该报错
        let result = transport
            .send(&"agent_x".into(), Message::Event {
                from: "agent_a".into(),
                channel: "test".into(),
                data: json!(null),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_publish_subscribe() {
        let transport = InMemoryTransport::new();

        // 注册三个 agent
        let mut rx_a = transport.subscribe(&"agent_a".into()).await.unwrap();
        let mut rx_b = transport.subscribe(&"agent_b".into()).await.unwrap();
        let mut rx_c = transport.subscribe(&"agent_c".into()).await.unwrap();

        // B 和 C 加入同一个频道
        transport.join_channel(&"agent_b".into(), "code_review").await.unwrap();
        transport.join_channel(&"agent_c".into(), "code_review").await.unwrap();

        // A 广播到频道
        transport
            .publish(
                "code_review",
                Message::Event {
                    from: "agent_a".into(),
                    channel: "code_review".into(),
                    data: json!({"file": "main.rs", "change": "modified"}),
                },
            )
            .await
            .unwrap();

        // B 和 C 都收到
        assert!(rx_b.recv().await.is_some());
        assert!(rx_c.recv().await.is_some());

        // A 没收到（没订阅频道）
        assert!(rx_a.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_leave_channel() {
        let transport = InMemoryTransport::new();

        let mut rx_b = transport.subscribe(&"agent_b".into()).await.unwrap();

        // 加入频道
        transport.join_channel(&"agent_b".into(), "test").await.unwrap();

        // 发布消息
        transport
            .publish(
                "test",
                Message::Event {
                    from: "other".into(),
                    channel: "test".into(),
                    data: json!({"test": 1}),
                },
            )
            .await
            .unwrap();

        // 应该收到
        assert!(rx_b.recv().await.is_some());

        // 离开频道
        transport.leave_channel(&"agent_b".into(), "test").await.unwrap();

        // 再发布消息
        transport
            .publish(
                "test",
                Message::Event {
                    from: "other".into(),
                    channel: "test".into(),
                    data: json!({"test": 2}),
                },
            )
            .await
            .unwrap();

        // 不应该收到
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_unsubscribe_cleanup() {
        let transport = InMemoryTransport::new();

        let _rx = transport.subscribe(&"agent_a".into()).await.unwrap();
        transport.join_channel(&"agent_a".into(), "test").await.unwrap();

        // 检查健康状态
        let health = transport.health_check();
        assert_eq!(health.agent_count, 1);
        assert_eq!(health.channel_count, 1);

        // 取消订阅
        transport.unsubscribe(&"agent_a".into()).await.unwrap();

        // 再次检查
        let health = transport.health_check();
        assert_eq!(health.agent_count, 0);
        assert_eq!(health.channel_count, 0); // 空频道被清理
    }

    #[tokio::test]
    async fn test_shutdown() {
        let transport = InMemoryTransport::new();

        let _rx_a = transport.subscribe(&"agent_a".into()).await.unwrap();
        let _rx_b = transport.subscribe(&"agent_b".into()).await.unwrap();
        transport.join_channel(&"agent_a".into(), "test").await.unwrap();

        // 关闭
        transport.shutdown().await;

        // 检查状态
        let health = transport.health_check();
        assert_eq!(health.agent_count, 0);
        assert_eq!(health.channel_count, 0);
        assert!(!transport.is_alive(&"agent_a".into()));
    }

    #[tokio::test]
    async fn test_health_check() {
        let transport = InMemoryTransport::new();

        // 初始状态
        let health = transport.health_check();
        assert!(health.healthy);
        assert_eq!(health.agent_count, 0);
        assert_eq!(health.channel_count, 0);

        // 添加 agents
        let _rx_a = transport.subscribe(&"agent_a".into()).await.unwrap();
        let _rx_b = transport.subscribe(&"agent_b".into()).await.unwrap();

        // 添加频道
        transport.join_channel(&"agent_a".into(), "channel_1").await.unwrap();
        transport.join_channel(&"agent_b".into(), "channel_1").await.unwrap();
        transport.join_channel(&"agent_a".into(), "channel_2").await.unwrap();

        // 检查健康状态
        let health = transport.health_check();
        assert!(health.healthy);
        assert_eq!(health.agent_count, 2);
        assert_eq!(health.channel_count, 2);
        assert!(health.agents.contains(&"agent_a".to_string()));
        assert!(health.agents.contains(&"agent_b".to_string()));
        assert!(health.channels.contains(&"channel_1".to_string()));
        assert!(health.channels.contains(&"channel_2".to_string()));
    }
}
