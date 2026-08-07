use super::message::{AgentFriendlyMessage, Intent, StandardMessage};

/// 消息格式化器：标准格式 → AI 友好格式。
///
/// 将结构化消息转换为自然语言描述，便于 Agent 理解。
#[allow(dead_code)]
pub struct MessageFormatter;

#[allow(dead_code)]
impl MessageFormatter {
    /// 标准消息 → AI 友好消息
    pub fn to_friendly(msg: &StandardMessage) -> AgentFriendlyMessage {
        let content = Self::generate_content(msg);
        let context = if msg.context.is_empty() {
            None
        } else {
            Some(
                msg.context
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect(),
            )
        };

        AgentFriendlyMessage {
            content,
            intent: Some(msg.intent.to_string()),
            expect: msg.expected_output.clone(),
            constraints: if msg.constraints.is_empty() {
                None
            } else {
                Some(msg.constraints.clone())
            },
            context,
        }
    }

    /// 生成自然语言内容
    fn generate_content(msg: &StandardMessage) -> String {
        match &msg.intent {
            Intent::TaskRequest => {
                let mut text = format!("{} 请求任务", msg.from);
                if let Some(file) = msg.context.get("file").and_then(|v| v.as_str()) {
                    text.push_str(&format!("（文件: {}）", file));
                }
                text.push_str("：");
                text.push_str(&msg.content);
                text
            }
            Intent::TaskResult => {
                format!("{} 完成任务，结果：{}", msg.from, msg.content)
            }
            Intent::TaskProgress => {
                format!("{} 进度更新：{}", msg.from, msg.content)
            }
            Intent::CodeReview => {
                let mut text = format!("{} 请求代码审查", msg.from);
                if let Some(file) = msg.context.get("file").and_then(|v| v.as_str()) {
                    text.push_str(&format!("（文件: {}）", file));
                }
                text.push_str("：");
                text.push_str(&msg.content);
                text
            }
            Intent::Question => {
                format!("{} 提问：{}", msg.from, msg.content)
            }
            Intent::Answer => {
                format!("{} 回答：{}", msg.from, msg.content)
            }
            Intent::CodeArtifact => {
                format!("{} 分享了代码：\n{}", msg.from, msg.content)
            }
            Intent::StatusUpdate => {
                format!("{} 状态更新：{}", msg.from, msg.content)
            }
            Intent::Discussion => {
                format!("{} 说：{}", msg.from, msg.content)
            }
            Intent::Unknown(_) => msg.content.clone(),
        }
    }

    /// 生成简短摘要（用于列表/通知）
    pub fn summarize(msg: &StandardMessage) -> String {
        let intent_label = match &msg.intent {
            Intent::TaskRequest => "📋 任务",
            Intent::TaskResult => "✅ 结果",
            Intent::TaskProgress => "📊 进度",
            Intent::CodeReview => "🔍 审查",
            Intent::Question => "❓ 提问",
            Intent::Answer => "💬 回答",
            Intent::CodeArtifact => "📄 代码",
            Intent::StatusUpdate => "🔄 状态",
            Intent::Discussion => "💭 讨论",
            Intent::Unknown(_) => "📨 消息",
        };

        let preview = if msg.content.len() > 50 {
            // 安全截断：在 UTF-8 字符边界处截断
            msg.content
                .char_indices()
                .take_while(|&(idx, _)| idx < 47)
                .map(|(_, c)| c)
                .collect::<String>()
                + "..."
        } else {
            msg.content.clone()
        };

        format!("{} {} → {}", intent_label, msg.from, preview)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Intent, Priority, StandardMessage};
    use std::collections::HashMap;

    fn standard(content: &str, intent: Intent) -> StandardMessage {
        StandardMessage {
            id: "test-001".to_string(),
            from: "developer".to_string(),
            to: Some("reviewer".to_string()),
            timestamp: 1000,
            intent,
            content: content.to_string(),
            expected_output: None,
            constraints: vec![],
            context: HashMap::new(),
            priority: Priority::Normal,
            task_id: None,
            reply_to: None,
        }
    }

    #[test]
    fn test_to_friendly_task_request() {
        let msg = standard("帮我实现登录功能", Intent::TaskRequest);
        let friendly = MessageFormatter::to_friendly(&msg);
        assert!(friendly.content.contains("developer"));
        assert!(friendly.content.contains("登录功能"));
        assert_eq!(friendly.intent, Some("task_request".to_string()));
    }

    #[test]
    fn test_to_friendly_code_review() {
        let mut msg = standard("请检查安全性", Intent::CodeReview);
        msg.context.insert(
            "file".to_string(),
            serde_json::Value::String("login.rs".to_string()),
        );
        let friendly = MessageFormatter::to_friendly(&msg);
        assert!(friendly.content.contains("login.rs"));
    }

    #[test]
    fn test_summarize() {
        let msg = standard("帮我实现登录功能", Intent::TaskRequest);
        let summary = MessageFormatter::summarize(&msg);
        assert!(summary.contains("📋"));
        assert!(summary.contains("developer"));
    }

    #[test]
    fn test_summarize_long_content() {
        let long_content = "a".repeat(100);
        let msg = standard(&long_content, Intent::Discussion);
        let summary = MessageFormatter::summarize(&msg);
        assert!(summary.contains("..."));
        assert!(summary.len() < 100);
    }
}
