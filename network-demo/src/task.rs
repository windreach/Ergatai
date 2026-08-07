use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::message::StandardMessage;
use super::transport::AgentId;

/// 任务状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskState {
    /// 刚创建，未分配
    Pending,
    /// 已分配给 agent
    Assigned { assignee: AgentId },
    /// 进行中
    InProgress { assignee: AgentId, progress: f32 },
    /// 已完成
    Completed { assignee: AgentId },
    /// 失败
    Failed { assignee: AgentId, reason: String },
}

#[allow(dead_code)]
impl TaskState {
    pub fn name(&self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Assigned { .. } => "assigned",
            TaskState::InProgress { .. } => "in_progress",
            TaskState::Completed { .. } => "completed",
            TaskState::Failed { .. } => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Completed { .. } | TaskState::Failed { .. })
    }

    pub fn assignee(&self) -> Option<&AgentId> {
        match self {
            TaskState::Assigned { assignee } => Some(assignee),
            TaskState::InProgress { assignee, .. } => Some(assignee),
            TaskState::Completed { assignee } => Some(assignee),
            TaskState::Failed { assignee, .. } => Some(assignee),
            TaskState::Pending => None,
        }
    }
}

/// 任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    pub request: StandardMessage,
    pub result: Option<StandardMessage>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// 任务管理器
#[allow(dead_code)]
pub struct TaskManager {
    tasks: RwLock<HashMap<String, Task>>,
}

#[allow(dead_code)]
impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
        }
    }

    /// 创建任务（状态: Pending）
    pub fn create_task(&self, msg: StandardMessage) -> String {
        let task_id = format!("task-{}", msg.id);
        let now = msg.timestamp;

        let task = Task {
            id: task_id.clone(),
            state: TaskState::Pending,
            request: msg,
            result: None,
            created_at: now,
            updated_at: now,
        };

        self.tasks.write().unwrap().insert(task_id.clone(), task);
        task_id
    }

    /// 分配任务给 agent（Pending → Assigned）
    pub fn assign_task(&self, task_id: &str, agent: &AgentId) -> Result<()> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {}", task_id))?;

        if task.state != TaskState::Pending {
            bail!("Task {} is not pending (current: {})", task_id, task.state.name());
        }

        task.state = TaskState::Assigned {
            assignee: agent.clone(),
        };
        task.updated_at = now_millis();
        Ok(())
    }

    /// 开始任务（Assigned → InProgress）
    pub fn start_task(&self, task_id: &str) -> Result<()> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {}", task_id))?;

        let assignee = match &task.state {
            TaskState::Assigned { assignee } => assignee.clone(),
            _ => bail!("Task {} is not assigned (current: {})", task_id, task.state.name()),
        };

        task.state = TaskState::InProgress {
            assignee,
            progress: 0.0,
        };
        task.updated_at = now_millis();
        Ok(())
    }

    /// 更新进度（InProgress only）
    pub fn update_progress(&self, task_id: &str, progress: f32) -> Result<()> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {}", task_id))?;

        match &mut task.state {
            TaskState::InProgress { progress: p, .. } => {
                *p = progress.clamp(0.0, 1.0);
                task.updated_at = now_millis();
                Ok(())
            }
            _ => bail!("Task {} is not in progress", task_id),
        }
    }

    /// 完成任务（InProgress → Completed）
    pub fn complete_task(&self, task_id: &str, result: StandardMessage) -> Result<()> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {}", task_id))?;

        let assignee = match &task.state {
            TaskState::InProgress { assignee, .. } => assignee.clone(),
            _ => bail!("Task {} is not in progress", task_id),
        };

        task.state = TaskState::Completed { assignee };
        task.result = Some(result);
        task.updated_at = now_millis();
        Ok(())
    }

    /// 标记失败
    pub fn fail_task(&self, task_id: &str, reason: &str) -> Result<()> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow::anyhow!("Task not found: {}", task_id))?;

        let assignee = task
            .state
            .assignee()
            .cloned()
            .unwrap_or_default();

        task.state = TaskState::Failed {
            assignee,
            reason: reason.to_string(),
        };
        task.updated_at = now_millis();
        Ok(())
    }

    /// 获取任务
    pub fn get_task(&self, task_id: &str) -> Option<Task> {
        self.tasks.read().unwrap().get(task_id).cloned()
    }

    /// 列出所有任务
    pub fn list_tasks(&self) -> Vec<Task> {
        self.tasks.read().unwrap().values().cloned().collect()
    }

    /// 列出指定 agent 的任务
    pub fn list_tasks_for_agent(&self, agent: &AgentId) -> Vec<Task> {
        self.tasks
            .read()
            .unwrap()
            .values()
            .filter(|t| t.state.assignee() == Some(agent))
            .cloned()
            .collect()
    }

    /// 列出指定状态的任务
    pub fn list_tasks_by_state(&self, state_name: &str) -> Vec<Task> {
        self.tasks
            .read()
            .unwrap()
            .values()
            .filter(|t| t.state.name() == state_name)
            .cloned()
            .collect()
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Intent, Priority, StandardMessage};
    use std::collections::HashMap;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static MSG_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn make_msg(content: &str) -> StandardMessage {
        let n = MSG_COUNTER.fetch_add(1, Ordering::SeqCst);
        StandardMessage {
            id: format!("msg-{:03}", n),
            from: "coordinator".to_string(),
            to: Some("developer".to_string()),
            timestamp: 1000,
            intent: Intent::TaskRequest,
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
    fn test_task_lifecycle() {
        let manager = TaskManager::new();

        // 创建
        let task_id = manager.create_task(make_msg("实现登录功能"));
        let task = manager.get_task(&task_id).unwrap();
        assert_eq!(task.state, TaskState::Pending);

        // 分配
        manager.assign_task(&task_id, &"developer".into()).unwrap();
        let task = manager.get_task(&task_id).unwrap();
        assert!(matches!(task.state, TaskState::Assigned { .. }));

        // 开始
        manager.start_task(&task_id).unwrap();
        let task = manager.get_task(&task_id).unwrap();
        assert!(matches!(task.state, TaskState::InProgress { progress, .. } if progress == 0.0));

        // 更新进度
        manager.update_progress(&task_id, 0.5).unwrap();
        let task = manager.get_task(&task_id).unwrap();
        assert!(matches!(task.state, TaskState::InProgress { progress, .. } if progress == 0.5));

        // 完成
        manager.complete_task(&task_id, make_msg("完成了")).unwrap();
        let task = manager.get_task(&task_id).unwrap();
        assert!(matches!(task.state, TaskState::Completed { .. }));
        assert!(task.result.is_some());
    }

    #[test]
    fn test_task_fail() {
        let manager = TaskManager::new();
        let task_id = manager.create_task(make_msg("实现功能"));
        manager.assign_task(&task_id, &"developer".into()).unwrap();
        manager.start_task(&task_id).unwrap();
        manager.fail_task(&task_id, "依赖缺失").unwrap();

        let task = manager.get_task(&task_id).unwrap();
        assert!(matches!(task.state, TaskState::Failed { reason, .. } if reason == "依赖缺失"));
    }

    #[test]
    fn test_invalid_transitions() {
        let manager = TaskManager::new();
        let task_id = manager.create_task(make_msg("test"));

        // Pending 不能直接开始
        assert!(manager.start_task(&task_id).is_err());

        // Pending 不能直接完成
        assert!(manager.complete_task(&task_id, make_msg("done")).is_err());

        // Pending 不能更新进度
        assert!(manager.update_progress(&task_id, 0.5).is_err());
    }

    #[test]
    fn test_list_tasks() {
        let manager = TaskManager::new();
        manager.create_task(make_msg("task 1"));
        manager.create_task(make_msg("task 2"));

        assert_eq!(manager.list_tasks().len(), 2);
    }
}
