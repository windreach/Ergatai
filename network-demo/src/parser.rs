use std::collections::HashMap;

use super::message::{
    AgentFriendlyMessage, Intent, Priority, StandardMessage, generate_message_id,
};
use super::transport::AgentId;

/// 消息解析器：AI 友好格式 → 标准格式。
///
/// 当前使用关键词匹配推断意图。后续可升级为 LLM 推断。
pub struct MessageParser;

impl MessageParser {
    /// 将 AI 友好消息解析为标准消息
    pub fn parse(msg: AgentFriendlyMessage, from: AgentId) -> StandardMessage {
        let intent = if let Some(ref intent_str) = msg.intent {
            Self::parse_intent(intent_str)
        } else {
            Self::infer_intent(&msg.content)
        };

        let context = if let Some(ctx) = msg.context {
            ctx.into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect()
        } else {
            Self::extract_context(&msg.content)
        };

        let priority = Self::infer_priority(&msg.content);

        StandardMessage {
            id: generate_message_id(&from),
            from,
            to: None,
            timestamp: now_millis(),
            intent,
            content: msg.content,
            expected_output: msg.expect,
            constraints: msg.constraints.unwrap_or_default(),
            context,
            priority,
            task_id: None,
            reply_to: None,
        }
    }

    /// 解析意图字符串
    fn parse_intent(s: &str) -> Intent {
        match s.to_lowercase().trim() {
            "task_request" | "task" => Intent::TaskRequest,
            "task_result" | "result" => Intent::TaskResult,
            "task_progress" | "progress" => Intent::TaskProgress,
            "question" | "ask" => Intent::Question,
            "answer" => Intent::Answer,
            "code_review" | "review" => Intent::CodeReview,
            "code_artifact" | "code" => Intent::CodeArtifact,
            "status_update" | "status" => Intent::StatusUpdate,
            "discussion" | "discuss" => Intent::Discussion,
            other => Intent::Unknown(other.to_string()),
        }
    }

    /// 从内容推断意图（关键词匹配）
    fn infer_intent(content: &str) -> Intent {
        let lower = content.to_lowercase();

        // 代码审查
        if contains_any(&lower, &["review", "审查", "检查代码", "code review", "看看这段代码"]) {
            return Intent::CodeReview;
        }

        // 任务结果
        if contains_any(&lower, &["完成了", "完成了任务", "结果如下", "已完成", "done", "result"]) {
            return Intent::TaskResult;
        }

        // 进度更新
        if contains_any(&lower, &["进度", "progress", "完成了%", "正在处理"]) {
            return Intent::TaskProgress;
        }

        // 提问
        if content.contains('？')
            || content.contains('?')
            || contains_any(&lower, &["怎么", "如何", "为什么", "是什么", "能不能", "可以吗"])
        {
            return Intent::Question;
        }

        // 代码分享
        if contains_any(&lower, &["```", "代码是", "代码如下", "这是代码", "function ", "fn ", "pub fn"])
        {
            return Intent::CodeArtifact;
        }

        // 状态更新
        if contains_any(&lower, &["状态", "status", "已启动", "已停止", "开始"]) {
            return Intent::StatusUpdate;
        }

        // 任务请求（默认）
        if contains_any(&lower, &["帮我", "请", "需要", "实现", "写", "创建", "please", "help"]) {
            return Intent::TaskRequest;
        }

        // 回答
        if contains_any(&lower, &["答案是", "回复", "回答", "我觉得", "建议"]) {
            return Intent::Answer;
        }

        // 默认：讨论
        Intent::Discussion
    }

    /// 从内容提取上下文
    fn extract_context(content: &str) -> HashMap<String, serde_json::Value> {
        let mut ctx = HashMap::new();

        // 提取文件路径（简单的启发式）
        let file_patterns = [".rs", ".ts", ".js", ".py", ".go", ".java", ".vue", ".html", ".css"];
        for word in content.split_whitespace() {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-');
            if file_patterns.iter().any(|ext| clean.ends_with(ext)) {
                ctx.insert("file".to_string(), serde_json::Value::String(clean.to_string()));
                break;
            }
        }

        // 提取函数名（简单的启发式）
        if let Some(idx) = content.find("fn ") {
            let rest = &content[idx + 3..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                ctx.insert("function".to_string(), serde_json::Value::String(name));
            }
        }

        ctx
    }

    /// 从内容推断优先级
    fn infer_priority(content: &str) -> Priority {
        let lower = content.to_lowercase();
        if contains_any(&lower, &["紧急", "urgent", "asap", "立即", "马上", "critical"]) {
            Priority::Urgent
        } else if contains_any(&lower, &["重要", "important", "高优先级", "high priority"]) {
            Priority::High
        } else if contains_any(&lower, &["不急", "low priority", "有空再看", "when you can"]) {
            Priority::Low
        } else {
            Priority::Normal
        }
    }
}

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| text.contains(p))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn friendly(content: &str) -> AgentFriendlyMessage {
        AgentFriendlyMessage {
            content: content.to_string(),
            intent: None,
            expect: None,
            constraints: None,
            context: None,
        }
    }

    #[test]
    fn test_infer_intent_code_review() {
        let msg = friendly("请帮我 review 这段代码");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.intent, Intent::CodeReview);
    }

    #[test]
    fn test_infer_intent_question() {
        let msg = friendly("这个函数怎么用？");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.intent, Intent::Question);
    }

    #[test]
    fn test_infer_intent_task_request() {
        let msg = friendly("帮我实现一个登录功能");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.intent, Intent::TaskRequest);
    }

    #[test]
    fn test_infer_intent_task_result() {
        let msg = friendly("完成了，结果如下");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.intent, Intent::TaskResult);
    }

    #[test]
    fn test_explicit_intent() {
        let msg = AgentFriendlyMessage {
            content: "随便说点什么".to_string(),
            intent: Some("code_review".to_string()),
            expect: None,
            constraints: None,
            context: None,
        };
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.intent, Intent::CodeReview);
    }

    #[test]
    fn test_extract_context_file() {
        let msg = friendly("请检查 src/main.rs 中的问题");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(
            parsed.context.get("file").and_then(|v| v.as_str()),
            Some("src/main.rs")
        );
    }

    #[test]
    fn test_infer_priority_urgent() {
        let msg = friendly("紧急修复线上 bug");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.priority, Priority::Urgent);
    }

    #[test]
    fn test_infer_priority_normal() {
        let msg = friendly("帮我写个工具函数");
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.priority, Priority::Normal);
    }

    #[test]
    fn test_constraints() {
        let msg = AgentFriendlyMessage {
            content: "帮我重构这个模块".to_string(),
            intent: None,
            expect: None,
            constraints: Some(vec![
                "不要修改公开 API".to_string(),
                "保持向后兼容".to_string(),
            ]),
            context: None,
        };
        let parsed = MessageParser::parse(msg, "agent_a".into());
        assert_eq!(parsed.constraints.len(), 2);
        assert!(parsed.constraints.contains(&"不要修改公开 API".to_string()));
    }
}
