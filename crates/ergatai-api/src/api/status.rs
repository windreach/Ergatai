use axum::{extract::State, response::IntoResponse, Json};
use ergatai_runtime::{get_agent_runtime, TmuxBackend, TmuxStatus};
use serde::Serialize;

use crate::{nats, AppState};

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub nats_initialized: bool,
    pub nats_port: Option<u16>,
    pub active_agents: usize,
    pub backend_info: Option<TmuxStatus>,
}

pub async fn get_status(State(_state): State<AppState>) -> impl IntoResponse {
    let runtime = get_agent_runtime();

    // Basic status
    let nats_initialized = nats::is_nats_initialized().await;
    let nats_port = nats::get_nats_server_port().await;
    let agents = runtime.list_agents().await;

    // Try to get TmuxBackend status
    let backend_info = {
        let backend = runtime.backend();
        let backend_any = backend.as_any();
        if let Some(tmux_backend) = backend_any.downcast_ref::<TmuxBackend>() {
            Some(tmux_backend.tmux_status().await)
        } else {
            None
        }
    };

    Json(StatusResponse {
        nats_initialized,
        nats_port,
        active_agents: agents.len(),
        backend_info,
    })
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_response_serialization_all_fields() {
        let resp = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 3,
            backend_info: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["nats_initialized"], true);
        assert_eq!(json["nats_port"], 4222);
        assert_eq!(json["active_agents"], 3);
        assert!(json["backend_info"].is_null());
    }

    #[test]
    fn test_status_response_serialization_no_nats() {
        let resp = StatusResponse {
            nats_initialized: false,
            nats_port: None,
            active_agents: 0,
            backend_info: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["nats_initialized"], false);
        assert!(json["nats_port"].is_null());
        assert_eq!(json["active_agents"], 0);
    }

    #[test]
    fn test_status_response_active_agents_varies() {
        for count in [0, 1, 5, 100] {
            let resp = StatusResponse {
                nats_initialized: true,
                nats_port: Some(4222),
                active_agents: count,
                backend_info: None,
            };
            let json = serde_json::to_value(&resp).unwrap();
            assert_eq!(json["active_agents"], count);
        }
    }

    #[test]
    fn test_status_response_json_shape_is_flat() {
        let resp = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 2,
            backend_info: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        // All 4 fields should be present at the top level
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert!(obj.contains_key("nats_initialized"));
        assert!(obj.contains_key("nats_port"));
        assert!(obj.contains_key("active_agents"));
        assert!(obj.contains_key("backend_info"));
    }

    #[test]
    fn test_status_response_nats_port_range() {
        // Valid port numbers should serialize correctly
        let resp = StatusResponse {
            nats_initialized: true,
            nats_port: Some(65535),
            active_agents: 0,
            backend_info: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["nats_port"], 65535);
    }
}
