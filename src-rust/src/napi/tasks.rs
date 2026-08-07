//! Cross-agent task coordination NAPI bindings.
//!
//! All `task_*` functions operate on the current working directory as the
//! project root. This matches how the Electron frontend invokes them.

use std::path::PathBuf;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use super::guard;

/// 检测消息中的跨 Agent 意图
#[napi]
pub fn cross_agent_detect_intent(content: String) -> Result<Option<String>> {
    guard();
    Ok(crate::cross_agent::detect_cross_agent_intent(&content))
}

// ── Task Coordinator (File-based Cross-Agent Collaboration) ──

fn project_root() -> napi::Result<PathBuf> {
    std::env::current_dir().map_err(|e| Error::from_reason(format!("cannot get cwd: {}", e)))
}

/// Create task plan file
#[napi]
pub async fn task_create_plan(task_id: String, content: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    coordinator
        .init()
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let plan_path = coordinator
        .create_plan(&task_id, &content)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(plan_path.to_string_lossy().into_owned())
}

/// Parse task plan file
#[napi]
pub async fn task_parse_plan(plan_file: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    let plan_path = PathBuf::from(&plan_file);
    let plan = coordinator
        .parse_plan(&plan_path)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    serde_json::to_string(&plan).map_err(|e| Error::from_reason(e.to_string()))
}

/// Create git worktree for agent
#[napi]
pub async fn task_create_worktree(task_id: String, agent: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    let worktree_path = coordinator
        .create_worktree(&task_id, &agent)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(worktree_path.to_string_lossy().into_owned())
}

/// Merge agent worktree to main
#[napi]
pub async fn task_merge_worktree(task_id: String, agent: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    let result = coordinator
        .merge_worktree(&task_id, &agent)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    serde_json::to_string(&result).map_err(|e| Error::from_reason(e.to_string()))
}

/// Check if all tasks in plan are completed
#[napi]
pub async fn task_check_completion(plan_file: String) -> Result<bool> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    let plan_path = PathBuf::from(&plan_file);
    let plan = coordinator
        .parse_plan(&plan_path)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    coordinator
        .check_completion(&plan)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// Clean up task worktrees and files
#[napi]
pub async fn task_cleanup(task_id: String) -> Result<()> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    coordinator
        .cleanup_task(&task_id)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// Get result file path for agent
#[napi]
pub fn task_get_result_path(task_id: String, agent: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root);
    let result_path = coordinator
        .get_result_path(&task_id, &agent)
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(result_path.to_string_lossy().into_owned())
}

// ── Agent Launcher ──

/// Launch all agents for a task plan
#[napi]
pub async fn task_launch_agents(plan_file: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root.clone());
    let plan_path = PathBuf::from(&plan_file);
    let plan = coordinator
        .parse_plan(&plan_path)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;

    let launcher = crate::cross_agent::AgentLauncher::new(root);
    let agent_ids = launcher
        .launch_agents(&plan)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;

    serde_json::to_string(&agent_ids).map_err(|e| Error::from_reason(e.to_string()))
}

/// Get status of all running agents
#[napi]
pub async fn task_get_agents_status() -> Result<String> {
    guard();
    let root = project_root()?;
    let launcher = crate::cross_agent::AgentLauncher::new(root);
    let agents = launcher.get_all_status().await;
    serde_json::to_string(&agents).map_err(|e| Error::from_reason(e.to_string()))
}

/// Check if all agents for a task are completed
#[napi]
pub async fn task_all_agents_completed(task_id: String) -> Result<bool> {
    guard();
    let root = project_root()?;
    let launcher = crate::cross_agent::AgentLauncher::new(root);
    Ok(launcher.all_agents_completed(&task_id).await)
}

/// Merge all completed agents for a task
#[napi]
pub async fn task_merge_all(task_id: String) -> Result<String> {
    guard();
    let root = project_root()?;
    let coordinator = crate::cross_agent::TaskCoordinator::new(root.clone());
    let launcher = crate::cross_agent::AgentLauncher::new(root);

    let agents = launcher.get_all_status().await;
    let task_agents: Vec<_> = agents
        .into_iter()
        .filter(|a| a.task_id == task_id)
        .collect();

    let mut results = Vec::new();
    for agent in task_agents {
        if agent.status == crate::cross_agent::AgentStatus::Completed {
            let result = coordinator
                .merge_worktree(&task_id, &agent.agent_name)
                .await
                .map_err(|e| Error::from_reason(e.to_string()))?;
            results.push(result);
        }
    }

    serde_json::to_string(&results).map_err(|e| Error::from_reason(e.to_string()))
}

// ── Task Scheduler (Multi-task Distribution) ──

/// Submit a task for scheduling (auto-starts background scheduler)
#[napi]
pub async fn task_schedule_submit(plan_file: String, _strategy: Option<String>) -> Result<String> {
    guard();
    let plan_path = PathBuf::from(&plan_file);

    // Get global scheduler (auto-starts background scheduler)
    let scheduler = crate::cross_agent::global_scheduler(None);
    let task_id = scheduler
        .submit_task(plan_path)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))?;
    Ok(task_id)
}

/// Check agent availability
#[napi]
pub async fn task_check_agent_status(agent_name: String) -> Result<String> {
    guard();

    let scheduler = crate::cross_agent::global_scheduler(None);
    let availability = scheduler.check_agent_availability(&agent_name).await;

    match availability {
        crate::cross_agent::AgentAvailability::Available => Ok("available".to_string()),
        crate::cross_agent::AgentAvailability::Busy { current_task_id } => {
            Ok(format!("busy:{}", current_task_id))
        }
        crate::cross_agent::AgentAvailability::NotRunning => Ok("not_running".to_string()),
    }
}

/// Get pending task count
#[napi]
pub async fn task_schedule_pending_count() -> Result<u32> {
    guard();
    let scheduler = crate::cross_agent::global_scheduler(None);
    let count = scheduler.pending_count().await;
    Ok(count as u32)
}

/// List pending tasks
#[napi]
pub async fn task_schedule_list_pending() -> Result<String> {
    guard();
    let scheduler = crate::cross_agent::global_scheduler(None);
    let tasks = scheduler.list_pending().await;
    serde_json::to_string(&tasks).map_err(|e| Error::from_reason(e.to_string()))
}

/// Cancel a pending task
#[napi]
pub async fn task_schedule_cancel(task_id: String) -> Result<bool> {
    guard();
    let scheduler = crate::cross_agent::global_scheduler(None);
    scheduler
        .cancel_task(&task_id)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// Mark task as completed (remove from processing)
#[napi]
pub async fn task_schedule_mark_completed(task_id: String) -> Result<()> {
    guard();
    let scheduler = crate::cross_agent::global_scheduler(None);
    scheduler.mark_completed(&task_id).await;
    Ok(())
}
