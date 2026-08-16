use axum::{extract::State, response::IntoResponse, Json};
use ergatai_runtime::{get_agent_runtime, RmuxBackend, RmuxDaemonInfo};
use serde::Serialize;

use crate::{nats, AppState};

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub nats_initialized: bool,
    pub nats_port: Option<u16>,
    pub active_agents: usize,
    pub daemon_info: Option<RmuxDaemonInfo>,
}

pub async fn get_status(State(_state): State<AppState>) -> impl IntoResponse {
    let runtime = get_agent_runtime();

    // Basic status
    let nats_initialized = nats::is_nats_initialized().await;
    let nats_port = nats::get_nats_server_port().await;
    let agents = runtime.list_agents().await;

    // Try to get RmuxBackend daemon info
    let daemon_info = {
        let backend = runtime.backend();
        // Check if backend is RmuxBackend using Any trait
        let backend_any = backend.as_any();
        if let Some(rmux_backend) = backend_any.downcast_ref::<RmuxBackend>() {
            Some(rmux_backend.daemon_info().await)
        } else {
            None
        }
    };

    Json(StatusResponse {
        nats_initialized,
        nats_port,
        active_agents: agents.len(),
        daemon_info,
    })
}
