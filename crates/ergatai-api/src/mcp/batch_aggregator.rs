//! BatchAggregator — 群发消息聚合器
//!
//! 当 agent A 在短时间内向多个 agent 发送消息时，收集所有回复并合并成一条消息返回，
//! 避免 A 重复处理多个单独的回复。
//!
//! ## 规则
//!
//! 1. **群发检测**: A 在 1 分钟内发给 ≥2 个不同 agent → 开启群发模式
//! 2. **回复窗口**: 每个目标 agent 从被发送时刻起有 1 分钟回复时间
//! 3. **超时计算**: 取所有目标中最大的超时时间 (last_send_time + 60s)
//! 4. **立即推送**: 收到回复数量 == 发送数量 → 立即合并推送
//! 5. **超时推送**: 超时后把已收集的回复合并推送
//! 6. **后续单独**: 超时后到达的回复 → 单独转发给原发送方
//!
//! ## 数据流
//!
//! ```text
//! Agent A → send_message(B), send_message(C), send_message(D)
//!                    ↓
//!         BatchAggregator 检测群发
//!         创建 BatchSession { targets: [B,C,D], timeout: T+60s }
//!                    ↓
//!         B/C/D 收到消息 (带 batch_id 标记)
//!                    ↓
//!         B/C/D 各自回复
//!                    ↓
//!         message_delivery 拦截回复 → BatchAggregator.on_reply()
//!                    ↓
//!         收齐 or 超时 → 合并成 1 条消息 → inject_message 给 A
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use ergatai_runtime::get_agent_runtime;

/// 群发检测时间窗口：1 分钟内发给多个不同 agent 视为群发
const BATCH_DETECTION_WINDOW: Duration = Duration::from_secs(60);

/// 每个目标 agent 的回复等待时间：从发送时刻起 1 分钟
const REPLY_WINDOW: Duration = Duration::from_secs(60);

/// 触发群发检测的最小目标数量
const MIN_BATCH_TARGETS: usize = 2;

/// 全局 BatchAggregator 实例
static BATCH_AGGREGATOR: OnceLock<Arc<BatchAggregator>> = OnceLock::new();

/// 获取全局 BatchAggregator 实例
pub fn get_batch_aggregator() -> Arc<BatchAggregator> {
    BATCH_AGGREGATOR
        .get_or_init(|| Arc::new(BatchAggregator::new()))
        .clone()
}

/// 群发消息聚合器
pub struct BatchAggregator {
    /// 活跃的群发 session，按 batch_id 索引
    sessions: Mutex<HashMap<String, BatchSession>>,
    /// 按 from_agent 索引的发送记录，用于检测群发
    /// Key: from_agent, Value: 最近的发送记录列表
    send_records: Mutex<HashMap<String, Vec<SendRecord>>>,
}

/// 单条发送记录
#[derive(Clone, Debug)]
struct SendRecord {
    /// 目标 agent
    to_agent: String,
    /// 发送时间
    sent_at: Instant,
    /// 所属 batch_id (如果已确定)
    batch_id: Option<String>,
}

/// 群发 session
#[derive(Debug)]
struct BatchSession {
    /// 唯一标识
    batch_id: String,
    /// 发送方 agent
    from_agent: String,
    /// 目标 agent 集合
    targets: HashSet<String>,
    /// 已收集的回复: to_agent → content
    replies: HashMap<String, String>,
    /// session 创建时间
    #[allow(dead_code)]
    created_at: Instant,
    /// 最大超时时间 (最后一个发送时刻 + REPLY_WINDOW)
    max_timeout: Instant,
    /// 是否已刷新 (合并回复已发送)
    flushed: bool,
}

impl BatchAggregator {
    /// 创建新的 BatchAggregator
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            send_records: Mutex::new(HashMap::new()),
        }
    }

    /// 记录一次发送事件，返回分配的 batch_id (如果是群发)
    ///
    /// 调用时机: server.rs 的 send_message 在发布消息前调用
    ///
    /// 参数:
    /// - `is_reply`: 是否是回复消息。回复消息不参与 batch 检测，避免混淆
    ///
    /// 返回值:
    /// - Some(batch_id): 该发送属于某个群发 session
    /// - None: 普通单发，不属于群发
    ///
    /// # 锁策略
    ///
    /// 分三阶段执行，避免长时间持有 `send_records` 锁：
    /// 1. 短暂持有 `send_records` 锁：检查已有 batch、添加新记录、提取 targets
    /// 2. 不持有任何锁：执行 batch 检测与创建（可能耗时较长）
    /// 3. 短暂持有 `send_records` 锁：更新记录的 batch_id
    pub async fn record_send(
        &self,
        from_agent: &str,
        to_agent: &str,
        is_reply: bool,
    ) -> Option<String> {
        // 回复消息不参与 batch 检测
        // 场景: A 发给 B/C (触发 batch)，B/C 回复后 A 再回复 B/C
        // 如果 A 的回复也被记录，会导致后续 batch 检测混乱
        if is_reply {
            debug!(
                from = from_agent,
                to = to_agent,
                "Skipping batch record for reply message"
            );
            return None;
        }

        let now = Instant::now();

        // 清理过期的发送记录
        self.cleanup_expired_records(from_agent, now).await;

        // ── Phase 1: 短暂持有 send_records 锁，提取 batch 检测所需数据 ──
        let recent_targets = {
            let mut records = self.send_records.lock().await;
            let agent_records = records.entry(from_agent.to_string()).or_default();

            // 检查是否已属于某个 batch
            for record in agent_records.iter() {
                if record.to_agent == to_agent && record.batch_id.is_some() {
                    return record.batch_id.clone();
                }
            }

            // 添加新记录
            agent_records.push(SendRecord {
                to_agent: to_agent.to_string(),
                sent_at: now,
                batch_id: None,
            });

            // 收集 1 分钟窗口内的不同目标（克隆出来，以便释放锁）
            agent_records
                .iter()
                .filter(|r| now.duration_since(r.sent_at) <= BATCH_DETECTION_WINDOW)
                .map(|r| r.to_agent.clone())
                .collect::<HashSet<_>>()
        }; // ← send_records 锁在此释放

        // ── Phase 2: 不持有 send_records 锁，执行 batch 检测/创建 ──
        let batch_id = self
            .check_and_create_batch(from_agent, &recent_targets, now)
            .await;

        // ── Phase 3: 短暂持有 send_records 锁，更新记录的 batch_id ──
        if let Some(ref bid) = batch_id {
            let mut records = self.send_records.lock().await;
            if let Some(agent_records) = records.get_mut(from_agent) {
                if let Some(record) = agent_records
                    .iter_mut()
                    .rev()
                    .find(|r| r.to_agent == to_agent)
                {
                    record.batch_id = Some(bid.clone());
                }
            }
        }

        batch_id
    }

    /// 检查是否触发群发，如果是则创建 BatchSession
    ///
    /// # 参数
    /// - `from_agent`: 发送方 agent ID
    /// - `recent_targets`: 已在调用方计算好的近期目标集合（避免持锁期间遍历）
    /// - `now`: 当前时间戳
    ///
    /// # 并发安全
    ///
    /// 检查与创建在同一个 `sessions` 锁作用域内完成，消除 TOCTOU 竞态。
    async fn check_and_create_batch(
        &self,
        from_agent: &str,
        recent_targets: &HashSet<String>,
        now: Instant,
    ) -> Option<String> {
        if recent_targets.len() < MIN_BATCH_TARGETS {
            return None;
        }

        // 整个检查和创建过程在同一个 sessions 锁作用域内完成，消除 TOCTOU 竞态。
        let mut sessions = self.sessions.lock().await;

        // 查找已有的活跃 batch
        let existing_batch = sessions
            .values_mut()
            .find(|s| s.from_agent == from_agent && !s.flushed);

        if let Some(session) = existing_batch {
            // 已有活跃 batch，检查是否需要扩展新目标
            let new_targets: Vec<_> = recent_targets
                .iter()
                .filter(|t| !session.targets.contains(*t))
                .cloned()
                .collect();

            if !new_targets.is_empty() {
                // 直接内联扩展（无需释放/重获锁，避免竞态窗口）
                for target in &new_targets {
                    session.targets.insert(target.clone());
                }
                let new_timeout = now + REPLY_WINDOW;
                if new_timeout > session.max_timeout {
                    session.max_timeout = new_timeout;
                }

                info!(
                    batch_id = %session.batch_id,
                    new_targets = ?new_targets,
                    all_targets = ?session.targets,
                    "Extended batch session"
                );

                return Some(session.batch_id.clone());
            }

            return Some(session.batch_id.clone());
        }

        // 无活跃 batch → 创建新 batch（仍在 sessions 锁持有期内，消除竞态窗口）
        Self::create_batch_locked(from_agent, recent_targets.clone(), now, &mut sessions)
    }

    /// 在已持有 `sessions` 锁的情况下创建新的 BatchSession。
    ///
    /// 此方法消除了原 `create_batch` 的 TOCTOU 竞态：检查与插入在同一个
    /// 锁作用域内完成，不可能有两个并发调用同时为同一 from_agent 创建 batch。
    fn create_batch_locked(
        from_agent: &str,
        targets: HashSet<String>,
        now: Instant,
        sessions: &mut tokio::sync::MutexGuard<'_, HashMap<String, BatchSession>>,
    ) -> Option<String> {
        // 二次检查：在等待锁期间可能已有其他任务创建了 batch
        for session in sessions.values() {
            if session.from_agent == from_agent && !session.flushed {
                return Some(session.batch_id.clone());
            }
        }

        // 生成 batch_id — 使用计数器（此处无 batch_counter 锁，使用静态原子计数器）
        static BATCH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = BATCH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let batch_id = format!("batch-{}-{}", from_agent, seq);

        let max_timeout = now + REPLY_WINDOW;

        let session = BatchSession {
            batch_id: batch_id.clone(),
            from_agent: from_agent.to_string(),
            targets,
            replies: HashMap::new(),
            created_at: now,
            max_timeout,
            flushed: false,
        };

        info!(
            batch_id = %batch_id,
            from = from_agent,
            targets = ?session.targets,
            timeout_secs = REPLY_WINDOW.as_secs(),
            "Created batch session"
        );

        sessions.insert(batch_id.clone(), session);

        // 启动超时检查任务（通过全局实例）
        get_batch_aggregator().spawn_timeout_checker(batch_id.clone());

        Some(batch_id)
    }

    /// 处理收到的回复
    ///
    /// 返回值:
    /// - Some(true): 回复已收集到 batch，不需要立即投递
    /// - Some(false): batch 已刷新，回复应单独投递
    /// - None: 不属于任何 batch，应直接投递
    pub async fn on_reply(&self, from_agent: &str, to_agent: &str, content: &str) -> Option<bool> {
        let mut sessions = self.sessions.lock().await;

        // 查找 from_agent 回复的目标 session
        // from_agent 是回复的发送方，to_agent 是回复的目标
        // 我们要找的是: to_agent 发起的 batch，且 from_agent 在 targets 中
        let mut batch_to_flush: Option<String> = None;

        for session in sessions.values_mut() {
            if session.from_agent == to_agent && session.targets.contains(from_agent) {
                if session.flushed {
                    // batch 已刷新，后续回复应单独投递
                    debug!(
                        batch_id = %session.batch_id,
                        from = from_agent,
                        "Batch already flushed, reply should be delivered individually"
                    );
                    return Some(false);
                }

                // 收集回复
                session
                    .replies
                    .insert(from_agent.to_string(), content.to_string());

                info!(
                    batch_id = %session.batch_id,
                    from = from_agent,
                    collected = session.replies.len(),
                    expected = session.targets.len(),
                    "Collected batch reply"
                );

                // 检查是否收齐
                if session.replies.len() >= session.targets.len() {
                    // 收齐，标记需要刷新
                    batch_to_flush = Some(session.batch_id.clone());
                }

                // 释放锁前返回（如果不需刷新）
                if batch_to_flush.is_none() {
                    return Some(true);
                }
                break;
            }
        }

        // 释放锁
        drop(sessions);

        // 如果需要刷新，执行刷新
        if let Some(batch_id) = batch_to_flush {
            self.flush_batch(&batch_id).await;
            return Some(true);
        }

        // 不属于任何 batch
        None
    }

    /// 刷新 batch：合并回复并发送给原发送方
    async fn flush_batch(&self, batch_id: &str) {
        let mut sessions = self.sessions.lock().await;
        let session = match sessions.get_mut(batch_id) {
            Some(s) => s,
            None => return,
        };

        if session.flushed {
            return;
        }

        if session.replies.is_empty() {
            info!(batch_id = %batch_id, "No replies collected, skipping flush");
            session.flushed = true;
            return;
        }

        // 合并回复
        let merged_content = self.merge_replies(&session.replies, &session.targets);
        let from_agent = session.from_agent.clone();

        session.flushed = true;

        info!(
            batch_id = %batch_id,
            from = from_agent,
            reply_count = session.replies.len(),
            "Flushing batch replies"
        );

        // 释放锁，避免死锁
        drop(sessions);

        // 投递合并后的消息
        let runtime = get_agent_runtime();
        match runtime.inject_message(&from_agent, &merged_content).await {
            Ok(()) => {
                info!(batch_id = %batch_id, to = from_agent, "Batch replies merged and delivered");
            }
            Err(e) => {
                warn!(batch_id = %batch_id, error = %e, "Failed to deliver merged batch replies");
            }
        }
    }

    /// 合并多条回复成一条消息（带去重）
    fn merge_replies(
        &self,
        replies: &HashMap<String, String>,
        targets: &HashSet<String>,
    ) -> String {
        let mut result = String::from("## 📨 Batch Reply Summary\n\n");

        // 按 targets 顺序排列，确保一致性
        let mut sorted_targets: Vec<_> = targets.iter().collect();
        sorted_targets.sort();

        // 用于去重：content -> list of agents who sent this content
        let mut content_to_agents: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        let _seen_contents: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 第一遍：收集所有回复，找出重复的
        for target in sorted_targets.iter() {
            if let Some(reply) = replies.get(target.as_str()) {
                // 清理提示词后再比较（避免相同内容因提示词不同被误判）
                let cleaned_reply = Self::strip_hint(reply);
                content_to_agents
                    .entry(cleaned_reply)
                    .or_default()
                    .push(target.as_str());
            }
        }

        // 第二遍：输出去重后的结果
        let mut output_order: Vec<(&str, &Vec<&str>)> =
            content_to_agents.iter().map(|(k, v)| (*k, v)).collect();
        // 按第一个 agent 的名称排序，保持一致性
        output_order.sort_by_key(|(_, agents)| agents.first().copied().unwrap_or(""));

        for (content, agents) in output_order {
            let from_list = agents.join(", ");
            if agents.len() > 1 {
                // 多个 agent 回复相同内容
                result.push_str(&format!(
                    "### From: {} (same reply)\n\n{}\n\n",
                    from_list, content
                ));
            } else {
                result.push_str(&format!("### From: {}\n\n{}\n\n", from_list, content));
            }
        }

        // 检查没有回复的 targets
        for target in sorted_targets.iter() {
            if !replies.contains_key(target.as_str()) {
                result.push_str(&format!("### From: {}\n\n_(no reply)_\n\n", target));
            }
        }

        result.push_str("---\n_Replies merged — no need to process individually_");
        result
    }

    /// 清理消息中的提示词，用于内容比较
    fn strip_hint(content: &str) -> &str {
        // 提示词格式: [Reply via send_message MCP...] 或 [If no questions, just show "Received"]
        // 这些提示词在消息末尾，用 [] 包裹
        let content = content.trim();
        if let Some(idx) = content.rfind('[') {
            let potential_hint = &content[idx..];
            if potential_hint.starts_with("[Reply via")
                || potential_hint.starts_with("[If no questions")
            {
                return content[..idx].trim();
            }
        }
        content
    }

    /// 启动超时检查任务
    fn spawn_timeout_checker(&self, batch_id: String) {
        tokio::spawn(async move {
            // 每 5 秒检查一次
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            loop {
                interval.tick().await;

                let should_flush = {
                    // 需要一个外部的 aggregator 引用
                    // 这里简化处理：通过全局实例获取
                    let aggregator = get_batch_aggregator();
                    let sessions = aggregator.sessions.lock().await;

                    match sessions.get(&batch_id) {
                        Some(session) => {
                            if session.flushed {
                                // 已刷新，退出检查
                                return;
                            }

                            let now = Instant::now();
                            now >= session.max_timeout
                        }
                        None => return, // session 不存在，退出
                    }
                };

                if should_flush {
                    info!(batch_id = %batch_id, "Batch timeout reached, flushing");
                    let aggregator = get_batch_aggregator();
                    aggregator.flush_batch(&batch_id).await;
                    return;
                }
            }
        });
    }

    /// 清理过期的发送记录
    async fn cleanup_expired_records(&self, from_agent: &str, now: Instant) {
        let mut records = self.send_records.lock().await;
        if let Some(agent_records) = records.get_mut(from_agent) {
            agent_records.retain(|r| now.duration_since(r.sent_at) <= BATCH_DETECTION_WINDOW);
        }
    }

    /// 获取指定 agent 的活跃 batch 信息 (用于调试)
    #[allow(dead_code)]
    pub async fn get_batch_info(&self, from_agent: &str) -> Vec<BatchInfo> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .filter(|s| s.from_agent == from_agent)
            .map(|s| BatchInfo {
                batch_id: s.batch_id.clone(),
                targets: s.targets.iter().cloned().collect(),
                replies_collected: s.replies.len(),
                flushed: s.flushed,
            })
            .collect()
    }
}

/// Batch 状态信息 (用于调试/API)
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct BatchInfo {
    pub batch_id: String,
    pub targets: Vec<String>,
    pub replies_collected: usize,
    pub flushed: bool,
}

impl Default for BatchAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_single_send_no_batch() {
        let aggregator = BatchAggregator::new();

        // 单次发送不应触发 batch
        let result = aggregator.record_send("agent-a", "agent-b", false).await;
        assert!(result.is_none(), "Single send should not create batch");
    }

    #[tokio::test]
    async fn test_two_sends_create_batch() {
        let aggregator = BatchAggregator::new();

        // 第一次发送
        let result1 = aggregator.record_send("agent-a", "agent-b", false).await;
        assert!(result1.is_none(), "First send should not create batch yet");

        // 第二次发送给不同目标
        let result2 = aggregator.record_send("agent-a", "agent-c", false).await;
        assert!(
            result2.is_some(),
            "Second send to different target should create batch"
        );

        // 验证 batch 信息
        let info = aggregator.get_batch_info("agent-a").await;
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].targets.len(), 2);
    }

    #[tokio::test]
    async fn test_same_target_no_batch() {
        let aggregator = BatchAggregator::new();

        // 多次发送给同一目标不应触发 batch
        aggregator.record_send("agent-a", "agent-b", false).await;
        let result = aggregator.record_send("agent-a", "agent-b", false).await;
        assert!(
            result.is_none(),
            "Sending to same target should not create batch"
        );
    }

    #[tokio::test]
    async fn test_reply_collection() {
        let aggregator = BatchAggregator::new();

        // 创建 batch: A → B, C
        aggregator.record_send("agent-a", "agent-b", false).await;
        let batch_id = aggregator.record_send("agent-a", "agent-c", false).await;
        assert!(batch_id.is_some());

        // B 回复 A
        let result1 = aggregator
            .on_reply("agent-b", "agent-a", "Hello from B")
            .await;
        assert_eq!(result1, Some(true), "Reply should be collected");

        // 检查状态
        let info = aggregator.get_batch_info("agent-a").await;
        assert_eq!(info[0].replies_collected, 1);

        // C 回复 A
        let result2 = aggregator
            .on_reply("agent-c", "agent-a", "Hello from C")
            .await;
        assert_eq!(result2, Some(true), "Reply should be collected");

        // 检查状态：应该已收齐
        let info = aggregator.get_batch_info("agent-a").await;
        assert_eq!(info[0].replies_collected, 2);
        // 注意：flush 是异步的，这里不检查 flushed 状态
    }

    #[tokio::test]
    async fn test_reply_not_in_batch() {
        let aggregator = BatchAggregator::new();

        // 没有 batch 的情况下，回复应返回 None
        let result = aggregator.on_reply("agent-x", "agent-y", "Hello").await;
        assert_eq!(result, None, "Reply without batch should return None");
    }

    #[tokio::test]
    async fn test_reply_send_not_recorded() {
        let aggregator = BatchAggregator::new();

        // 回复消息不应被记录到 batch 检测中
        let result = aggregator.record_send("agent-a", "agent-b", true).await;
        assert!(result.is_none(), "Reply send should not create batch");

        // 再发一条回复，也不应该触发 batch
        let result2 = aggregator.record_send("agent-a", "agent-c", true).await;
        assert!(
            result2.is_none(),
            "Reply sends should not create batch even to multiple targets"
        );

        // 检查 send_records 应该是空的
        let records = aggregator.send_records.lock().await;
        assert!(
            records.get("agent-a").is_none() || records.get("agent-a").unwrap().is_empty(),
            "Reply sends should not be recorded"
        );
    }

    #[tokio::test]
    async fn test_get_batch_info_empty() {
        let aggregator = BatchAggregator::new();
        let info = aggregator.get_batch_info("agent-a").await;
        assert!(info.is_empty(), "No batches initially");
    }

    #[tokio::test]
    async fn test_get_batch_info_with_active_batch() {
        let aggregator = BatchAggregator::new();

        // Create a batch by sending to 2+ agents
        aggregator.record_send("agent-a", "agent-b", false).await;
        aggregator.record_send("agent-a", "agent-c", false).await;

        let info = aggregator.get_batch_info("agent-a").await;
        assert_eq!(info.len(), 1, "Should have one active batch");
        assert!(info[0].targets.contains(&"agent-b".to_string()));
        assert!(info[0].targets.contains(&"agent-c".to_string()));
        assert_eq!(info[0].replies_collected, 0);
        assert!(!info[0].flushed);
    }

    #[tokio::test]
    async fn test_multiple_agents_independent_batches() {
        let aggregator = BatchAggregator::new();

        // Agent A sends to B and C (batch for A)
        aggregator.record_send("agent-a", "agent-b", false).await;
        aggregator.record_send("agent-a", "agent-c", false).await;

        // Agent X sends to Y and Z (batch for X)
        aggregator.record_send("agent-x", "agent-y", false).await;
        aggregator.record_send("agent-x", "agent-z", false).await;

        let info_a = aggregator.get_batch_info("agent-a").await;
        let info_x = aggregator.get_batch_info("agent-x").await;
        assert_eq!(info_a.len(), 1);
        assert_eq!(info_x.len(), 1);
        assert!(info_a[0].targets.contains(&"agent-b".to_string()));
        assert!(info_x[0].targets.contains(&"agent-y".to_string()));
    }

    #[tokio::test]
    async fn test_on_reply_collects_multiple_replies() {
        let aggregator = BatchAggregator::new();

        // Create batch: A → B, C
        aggregator.record_send("agent-a", "agent-b", false).await;
        aggregator.record_send("agent-a", "agent-c", false).await;

        // B replies — should be accepted (Some(true))
        let r1 = aggregator.on_reply("agent-b", "agent-a", "reply from B").await;
        assert_eq!(r1, Some(true), "First reply should be accepted");

        // C replies — should also be accepted (finalizes the batch)
        let r2 = aggregator.on_reply("agent-c", "agent-a", "reply from C").await;
        assert_eq!(r2, Some(true), "Second reply should also be accepted");

        // After flush, further replies should be delivered individually (Some(false))
        let r3 = aggregator.on_reply("agent-b", "agent-a", "follow-up from B").await;
        assert_eq!(
            r3,
            Some(false),
            "Reply after flush should indicate individual delivery"
        );
    }

    #[tokio::test]
    async fn test_on_reply_unknown_batch_returns_none() {
        let aggregator = BatchAggregator::new();
        // Reply from agent that's not in any batch
        let result = aggregator.on_reply("stranger", "agent-a", "hi").await;
        assert_eq!(result, None);
    }
}
