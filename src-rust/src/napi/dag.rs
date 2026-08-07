//! DAG Scheduler NAPI bindings.
//!
//! Exposes the DAG-based multi-agent orchestration to JS.
//! Flow: dag_submit(markdown) → (auto-completion via ACP) → dag_progress/dag_is_complete queries
//!
//! Note: dag_node_completed/dag_node_failed are no longer needed — when an ACP session
//! for a DAG node completes, agent_launcher.rs auto-notifies DagScheduler via spawn_blocking.

use std::path::PathBuf;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use super::guard;

fn project_root() -> napi::Result<PathBuf> {
    std::env::current_dir().map_err(|e| Error::from_reason(format!("cannot get cwd: {e}")))
}

/// Submit a DAG for execution.
///
/// Parses the Markdown DAG spec, creates a DagScheduler, stores it globally,
/// and submits all ready (no-dependency) nodes to the task scheduler.
///
/// Returns JSON array of submitted task IDs.
#[napi]
pub async fn dag_submit(markdown: String) -> Result<String> {
    guard();
    let root = project_root()?;

    let graph = crate::orchestration::dag_parser::parse_dag_markdown(&markdown)
        .map_err(|e| Error::from_reason(format!("Failed to parse DAG markdown: {e}")))?;

    let scheduler = crate::cross_agent::DagScheduler::new(root, graph);
    let submitted = scheduler
        .submit_graph()
        .await
        .map_err(|e| Error::from_reason(format!("Failed to submit DAG: {e}")))?;

    // Store globally for subsequent callbacks
    crate::cross_agent::set_dag_scheduler(scheduler);

    serde_json::to_string(&submitted).map_err(|e| Error::from_reason(e.to_string()))
}

/// Get DAG progress as a value between 0.0 and 1.0.
#[napi]
pub async fn dag_progress() -> Result<f64> {
    guard();

    let scheduler = crate::cross_agent::get_dag_scheduler()
        .ok_or_else(|| Error::from_reason("No active DAG scheduler."))?;

    Ok(scheduler.progress().await as f64)
}

/// Check if all nodes in the DAG are complete (Completed, Failed, or Skipped).
#[napi]
pub async fn dag_is_complete() -> Result<bool> {
    guard();

    let scheduler = crate::cross_agent::get_dag_scheduler()
        .ok_or_else(|| Error::from_reason("No active DAG scheduler."))?;

    Ok(scheduler.is_complete().await)
}

/// Get AI-friendly status text for the current DAG.
#[napi]
pub async fn dag_status() -> Result<String> {
    guard();

    let scheduler = crate::cross_agent::get_dag_scheduler()
        .ok_or_else(|| Error::from_reason("No active DAG scheduler."))?;

    Ok(scheduler.status_prompt().await)
}

/// Get full DAG state as JSON (nodes, statuses, dependencies).
#[napi]
pub async fn dag_get_state() -> Result<String> {
    guard();

    let scheduler = crate::cross_agent::get_dag_scheduler()
        .ok_or_else(|| Error::from_reason("No active DAG scheduler."))?;

    scheduler
        .graph_snapshot()
        .await
        .map_err(|e| Error::from_reason(format!("Failed to get DAG state: {e}")))
}
