use std::collections::HashMap;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use ergatai_runtime::{WorkspaceSpec, ResourceLimits, get_agent_runtime};

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct SpawnAgentRequest {
    pub workspace_id: String,
    pub command: String,
    pub instruction: Option<String>,
    pub work_dir: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct SpawnAgentResponse {
    pub agent_id: String,
}

#[derive(Debug, Serialize)]
pub struct AgentInfoResponse {
    pub agent_id: String,
    pub workspace_id: String,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub async fn list_agents(
    State(_state): State<AppState>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();
    let agents = runtime.list_agents().await;

    let response: Vec<AgentInfoResponse> = agents
        .into_iter()
        .map(|a| AgentInfoResponse {
            agent_id: a.agent_id,
            workspace_id: a.workspace_id,
            state: format!("{:?}", a.state),
            created_at: a.created_at.to_rfc3339(),
        })
        .collect();

    Json(response)
}

pub async fn spawn_agent(
    State(state): State<AppState>,
    Json(req): Json<SpawnAgentRequest>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();
    let spec = WorkspaceSpec {
        id: req.workspace_id,
        work_dir: req
            .work_dir
            .unwrap_or_else(|| state.default_cwd.clone())
            .into(),
        env: req.env.unwrap_or_default(),
        resources: ResourceLimits::default(),
        backend_config: serde_json::json!({}),
    };

    match runtime
        .launch_agent(spec, &req.command, req.instruction.as_deref())
        .await
    {
        Ok(agent_id) => (
            StatusCode::CREATED,
            Json(SpawnAgentResponse { agent_id }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}

pub async fn kill_agent(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();

    match runtime.stop_agent(&id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}

pub async fn send_message(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();

    match runtime.inject_message(&id, &req.message).await {
        Ok(_) => StatusCode::OK.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}
