use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::transport::AgentId;

// ── 外层：AI 友好格式 ──

/// Agent 发送/接收的消息（AI 友好，自然语言为主）。
///
/// Agent 只需关注 `content`，其余字段可选。
/// 系统会自动推断 intent、提取 context。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFriendlyMessage {
    /// 消息内容（自然语言）
    pub content: String,

    /// 意图（可选，不填则系统推断）
    pub intent: Option<String>,

    /// 期望对方输出什么（可选）
    pub expect: Option<String>,

    /// 约束条件（可选）
    pub constraints: Option<Vec<String>>,

    /// 上下文（可选）
    pub context: Option<HashMap<String, String>>,
}

// ── 内层：标准格式 ──

/// 系统内部标准消息（结构化，用于路由/持久化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandardMessage {
    /// 消息 ID
    pub id: String,

    /// 发送者
    pub from: AgentId,

    /// 接收者（None = 广播）
    pub to: Option<AgentId>,

    /// 时间戳（毫秒）
    pub timestamp: u64,

    /// 意图
    pub intent: Intent,

    /// 消息内容
    pub content: String,

    /// 期望输出
    pub expected_output: Option<String>,

    /// 约束条件
    pub constraints: Vec<String>,

    /// 上下文
    pub context: HashMap<String, serde_json::Value>,

    /// 优先级
    pub priority: Priority,

    /// 关联任务 ID
    pub task_id: Option<String>,

    /// 回复的消息 ID
    pub reply_to: Option<String>,
}

// ── 意图 ──

/// 消息意图
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Intent {
    /// 请求执行任务
    TaskRequest,
    /// 任务完成结果
    TaskResult,
    /// 任务进度更新
    TaskProgress,
    /// 提问
    Question,
    /// 回答
    Answer,
    /// 代码审查
    CodeReview,
    /// 代码/文件分享
    CodeArtifact,
    /// 状态更新
    StatusUpdate,
    /// 讨论/协商
    Discussion,
    /// 未识别
    Unknown(String),
}

impl Intent {
    pub fn name(&self) -> &str {
        match self {
            Intent::TaskRequest => "task_request",
            Intent::TaskResult => "task_result",
            Intent::TaskProgress => "task_progress",
            Intent::Question => "question",
            Intent::Answer => "answer",
            Intent::CodeReview => "code_review",
            Intent::CodeArtifact => "code_artifact",
            Intent::StatusUpdate => "status_update",
            Intent::Discussion => "discussion",
            Intent::Unknown(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// ── 优先级 ──

/// 消息优先级
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Normal
    }
}

#[allow(dead_code)]
impl Priority {
    pub fn name(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }
}

// ── 工具函数 ──

/// 生成消息 ID
pub fn generate_message_id(from: &AgentId) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // 使用纳秒时间戳 + 随机数确保唯一性
    let random_part: u32 = std::hash::BuildHasher::hash_one(
        &std::collections::hash_map::RandomState::new(),
        &ts,
    ) as u32;
    format!("{}-{}-{:08x}", from, ts / 1_000_000, random_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intent_display() {
        assert_eq!(Intent::TaskRequest.to_string(), "task_request");
        assert_eq!(Intent::CodeReview.to_string(), "code_review");
        assert_eq!(Intent::Unknown("custom".into()).to_string(), "custom");
    }

    #[test]
    fn test_priority_default() {
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn test_message_id_format() {
        let id = generate_message_id(&"agent_a".to_string());
        assert!(id.starts_with("agent_a-"));
    }
}
