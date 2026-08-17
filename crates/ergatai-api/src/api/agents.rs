use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use ergatai_runtime::{get_agent_runtime, ResourceLimits, WorkspaceSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

pub async fn list_agents(State(_state): State<AppState>) -> impl IntoResponse {
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

/// Allowed commands for agent spawning (security whitelist).
/// Only these commands can be executed via the API to prevent arbitrary command execution.
const ALLOWED_COMMANDS: &[&str] = &["claude", "cursor", "codex", "ergatai-agent", "simple-agent"];

/// Validate that a workspace_id contains only safe characters (alphanumeric, hyphens, underscores).
/// This prevents path traversal (`..`), absolute paths (`/etc`), and shell injection.
fn is_valid_workspace_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validate that the command is in the allowed list.
fn validate_command(command: &str) -> Result<(), String> {
    let program = command
        .split_whitespace()
        .next()
        .ok_or_else(|| "Empty command".to_string())?;

    // Extract just the binary name (handle paths like /usr/bin/claude)
    let binary_name = std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);

    if ALLOWED_COMMANDS.contains(&binary_name) {
        Ok(())
    } else {
        // Security: don't leak the whitelist to clients
        Err(format!("Command '{}' is not allowed", binary_name))
    }
}

pub async fn spawn_agent(
    State(state): State<AppState>,
    Json(req): Json<SpawnAgentRequest>,
) -> impl IntoResponse {
    // Security: validate command against whitelist before execution
    if let Err(e) = validate_command(&req.command) {
        return (StatusCode::FORBIDDEN, Json(ErrorResponse { error: e })).into_response();
    }

    // Security: validate workspace_id contains only safe characters
    if !is_valid_workspace_id(&req.workspace_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "workspace_id must contain only alphanumeric characters, hyphens, or underscores".to_string(),
            }),
        )
            .into_response();
    }

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
        Ok(agent_id) => {
            (StatusCode::CREATED, Json(SpawnAgentResponse { agent_id })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
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
            Json(ErrorResponse {
                error: e.to_string(),
            }),
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
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── SpawnAgentRequest deserialization ──

    #[test]
    fn test_spawn_agent_request_required_fields() {
        let req: SpawnAgentRequest = serde_json::from_value(json!({
            "workspace_id": "ws-1",
            "command": "claude"
        }))
        .unwrap();
        assert_eq!(req.workspace_id, "ws-1");
        assert_eq!(req.command, "claude");
        assert!(req.instruction.is_none());
        assert!(req.work_dir.is_none());
        assert!(req.env.is_none());
    }

    #[test]
    fn test_spawn_agent_request_all_fields() {
        let req: SpawnAgentRequest = serde_json::from_value(json!({
            "workspace_id": "ws-1",
            "command": "claude",
            "instruction": "do a task",
            "work_dir": "/tmp/work",
            "env": {"FOO": "bar", "BAZ": "qux"}
        }))
        .unwrap();
        assert_eq!(req.instruction.as_deref(), Some("do a task"));
        assert_eq!(req.work_dir.as_deref(), Some("/tmp/work"));
        let env = req.env.unwrap();
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn test_spawn_agent_request_missing_workspace_id_fails() {
        let result: Result<SpawnAgentRequest, _> =
            serde_json::from_value(json!({"command": "claude"}));
        assert!(result.is_err());
    }

    #[test]
    fn test_spawn_agent_request_missing_command_fails() {
        let result: Result<SpawnAgentRequest, _> =
            serde_json::from_value(json!({"workspace_id": "ws-1"}));
        assert!(result.is_err());
    }

    #[test]
    fn test_spawn_agent_request_null_optional_fields() {
        let req: SpawnAgentRequest = serde_json::from_value(json!({
            "workspace_id": "ws-1",
            "command": "claude",
            "instruction": null,
            "work_dir": null,
            "env": null
        }))
        .unwrap();
        assert!(req.instruction.is_none());
        assert!(req.work_dir.is_none());
        assert!(req.env.is_none());
    }

    // ── SendMessageRequest deserialization ──

    #[test]
    fn test_send_message_request_valid() {
        let req: SendMessageRequest =
            serde_json::from_value(json!({"message": "hello world"})).unwrap();
        assert_eq!(req.message, "hello world");
    }

    #[test]
    fn test_send_message_request_empty_string() {
        let req: SendMessageRequest = serde_json::from_value(json!({"message": ""})).unwrap();
        assert_eq!(req.message, "");
    }

    #[test]
    fn test_send_message_request_missing_message_fails() {
        let result: Result<SendMessageRequest, _> = serde_json::from_value(json!({}));
        assert!(result.is_err());
    }

    // ── Response struct serialization ──

    #[test]
    fn test_spawn_agent_response_serialization() {
        let resp = SpawnAgentResponse {
            agent_id: "agent-42".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["agent_id"], "agent-42");
    }

    #[test]
    fn test_agent_info_response_serialization() {
        let resp = AgentInfoResponse {
            agent_id: "a-1".to_string(),
            workspace_id: "ws-1".to_string(),
            state: "Running".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["agent_id"], "a-1");
        assert_eq!(json["workspace_id"], "ws-1");
        assert_eq!(json["state"], "Running");
        assert_eq!(json["created_at"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_error_response_serialization() {
        let resp = ErrorResponse {
            error: "something went wrong".to_string(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["error"], "something went wrong");
        // Only one field
        assert_eq!(json.as_object().unwrap().len(), 1);
    }
}
