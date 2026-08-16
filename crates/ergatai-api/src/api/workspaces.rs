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
pub struct CreateWorkspaceRequest {
    pub id: String,
    pub work_dir: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub id: String,
    pub backend: String,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub async fn list_workspaces(
    State(_state): State<AppState>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();
    match runtime.backend().list_workspaces().await {
        Ok(workspaces) => {
            let response: Vec<WorkspaceResponse> = workspaces
                .into_iter()
                .map(|w| WorkspaceResponse {
                    id: w.id,
                    backend: w.backend,
                    metadata: w.metadata,
                })
                .collect();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}

pub async fn create_workspace(
    State(state): State<AppState>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();
    let spec = WorkspaceSpec {
        id: req.id,
        work_dir: req
            .work_dir
            .unwrap_or_else(|| state.default_cwd.clone())
            .into(),
        env: req.env.unwrap_or_default(),
        resources: ResourceLimits::default(),
        backend_config: serde_json::json!({}),
    };

    match runtime.backend().create_workspace(spec).await {
        Ok(handle) => {
            let response = WorkspaceResponse {
                id: handle.id,
                backend: handle.backend,
                metadata: handle.metadata,
            };
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}

pub async fn delete_workspace(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let runtime = get_agent_runtime();

    // Find workspace by ID
    let workspaces = match runtime.backend().list_workspaces().await {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e.to_string() }),
            )
                .into_response()
        }
    };

    let workspace = match workspaces.into_iter().find(|w| w.id == id) {
        Some(w) => w,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Workspace {} not found", id),
                }),
            )
                .into_response()
        }
    };

    match runtime.backend().cleanup_workspace(&workspace).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e.to_string() }),
        )
            .into_response(),
    }
}
