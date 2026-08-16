use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub struct ErgataiClient {
    client: Client,
    base_url: String,
    token: Option<String>,
}

impl ErgataiClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            token: token.map(|t| t.to_string()),
        }
    }

    fn add_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.token {
            req.bearer_auth(token)
        } else {
            req
        }
    }

    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceResponse>> {
        let url = format!("{}/api/v1/workspaces", self.base_url);
        let req = self.client.get(&url);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(response.json().await?)
    }

    pub async fn create_workspace(
        &self,
        id: &str,
        work_dir: Option<&str>,
    ) -> Result<WorkspaceResponse> {
        let url = format!("{}/api/v1/workspaces", self.base_url);
        let body = CreateWorkspaceRequest {
            id: id.to_string(),
            work_dir: work_dir.map(|s| s.to_string()),
        };
        let req = self.client.post(&url).json(&body);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(response.json().await?)
    }

    pub async fn delete_workspace(&self, id: &str) -> Result<()> {
        let url = format!("{}/api/v1/workspaces/{}", self.base_url, id);
        let req = self.client.delete(&url);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(())
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentInfoResponse>> {
        let url = format!("{}/api/v1/agents", self.base_url);
        let req = self.client.get(&url);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(response.json().await?)
    }

    pub async fn spawn_agent(
        &self,
        workspace_id: &str,
        command: &str,
        instruction: Option<&str>,
    ) -> Result<SpawnAgentResponse> {
        let url = format!("{}/api/v1/agents", self.base_url);
        let body = SpawnAgentRequest {
            workspace_id: workspace_id.to_string(),
            command: command.to_string(),
            instruction: instruction.map(|s| s.to_string()),
        };
        let req = self.client.post(&url).json(&body);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(response.json().await?)
    }

    pub async fn kill_agent(&self, id: &str) -> Result<()> {
        let url = format!("{}/api/v1/agents/{}", self.base_url, id);
        let req = self.client.delete(&url);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(())
    }

    pub async fn send_message(&self, id: &str, message: &str) -> Result<()> {
        let url = format!("{}/api/v1/agents/{}/message", self.base_url, id);
        let body = SendMessageRequest {
            message: message.to_string(),
        };
        let req = self.client.post(&url).json(&body);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(())
    }

    pub async fn get_status(&self) -> Result<StatusResponse> {
        let url = format!("{}/api/v1/status", self.base_url);
        let req = self.client.get(&url);
        let req = self.add_auth(req);
        let response = req.send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            anyhow::bail!("API error: {}", error_text);
        }

        Ok(response.json().await?)
    }
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceResponse {
    pub id: String,
    pub backend: String,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentInfoResponse {
    pub agent_id: String,
    pub workspace_id: String,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SpawnAgentResponse {
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub nats_initialized: bool,
    pub nats_port: Option<u16>,
    pub active_agents: usize,
    pub daemon_info: Option<DaemonInfo>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonInfo {
    pub binary_available: bool,
    pub binary_path: Option<String>,
    pub connected: bool,
    pub tracked_pane_count: usize,
    pub tracked_workspace_count: usize,
    pub total_sessions: usize,
    pub total_daemon_panes: usize,
    pub ergatai_sessions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CreateWorkspaceRequest {
    id: String,
    work_dir: Option<String>,
}

#[derive(Debug, Serialize)]
struct SpawnAgentRequest {
    workspace_id: String,
    command: String,
    instruction: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    message: String,
}
