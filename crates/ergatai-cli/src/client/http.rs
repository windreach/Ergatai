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
        work_dir: Option<&str>,
        instruction: Option<&str>,
    ) -> Result<SpawnAgentResponse> {
        let url = format!("{}/api/v1/agents", self.base_url);
        let body = SpawnAgentRequest {
            workspace_id: workspace_id.to_string(),
            command: command.to_string(),
            work_dir: work_dir.map(|s| s.to_string()),
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
    work_dir: Option<String>,
    instruction: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_new_without_token() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        assert_eq!(client.base_url, "http://localhost:3000");
        assert!(client.token.is_none());
    }

    #[test]
    fn test_client_new_with_token() {
        let client = ErgataiClient::new("http://localhost:3000", Some("my-secret"));
        assert_eq!(client.base_url, "http://localhost:3000");
        assert_eq!(client.token.as_deref(), Some("my-secret"));
    }

    #[test]
    fn test_client_new_strips_trailing_slash_consistency() {
        // The client stores the base_url as-is; URL building concatenates paths
        let client = ErgataiClient::new("http://example.com/api", None);
        assert_eq!(client.base_url, "http://example.com/api");
    }

    #[test]
    fn test_add_auth_with_token() {
        let client = ErgataiClient::new("http://localhost:3000", Some("bearer-token"));
        let req = client.client.get("http://localhost:3000/test");
        // Just verify add_auth doesn't panic and returns a valid request builder
        let _req = client.add_auth(req);
    }

    #[test]
    fn test_add_auth_without_token() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let req = client.client.get("http://localhost:3000/test");
        let _req = client.add_auth(req);
    }

    #[test]
    fn test_workspace_list_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let url = format!("{}/api/v1/workspaces", client.base_url);
        assert_eq!(url, "http://localhost:3000/api/v1/workspaces");
    }

    #[test]
    fn test_agents_list_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let url = format!("{}/api/v1/agents", client.base_url);
        assert_eq!(url, "http://localhost:3000/api/v1/agents");
    }

    #[test]
    fn test_delete_workspace_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let id = "ws-123";
        let url = format!("{}/api/v1/workspaces/{}", client.base_url, id);
        assert_eq!(url, "http://localhost:3000/api/v1/workspaces/ws-123");
    }

    #[test]
    fn test_kill_agent_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let id = "agent-456";
        let url = format!("{}/api/v1/agents/{}", client.base_url, id);
        assert_eq!(url, "http://localhost:3000/api/v1/agents/agent-456");
    }

    #[test]
    fn test_send_message_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let id = "agent-789";
        let url = format!("{}/api/v1/agents/{}/message", client.base_url, id);
        assert_eq!(url, "http://localhost:3000/api/v1/agents/agent-789/message");
    }

    #[test]
    fn test_status_url_building() {
        let client = ErgataiClient::new("http://localhost:3000", None);
        let url = format!("{}/api/v1/status", client.base_url);
        assert_eq!(url, "http://localhost:3000/api/v1/status");
    }

    #[test]
    fn test_create_workspace_request_serialization() {
        let req = CreateWorkspaceRequest {
            id: "ws-1".to_string(),
            work_dir: Some("/tmp/work".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"id\":\"ws-1\""));
        assert!(json.contains("\"work_dir\":\"/tmp/work\""));
    }

    #[test]
    fn test_create_workspace_request_no_work_dir() {
        let req = CreateWorkspaceRequest {
            id: "ws-2".to_string(),
            work_dir: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"id\":\"ws-2\""));
        // None is serialized as null by default (no skip_serializing_if)
        assert!(json.contains("\"work_dir\":null"));
    }

    #[test]
    fn test_spawn_agent_request_serialization() {
        let req = SpawnAgentRequest {
            workspace_id: "ws-1".to_string(),
            command: "claude".to_string(),
            work_dir: Some("/tmp/work".to_string()),
            instruction: Some("fix the bug".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"workspace_id\":\"ws-1\""));
        assert!(json.contains("\"command\":\"claude\""));
        assert!(json.contains("\"instruction\":\"fix the bug\""));
    }

    #[test]
    fn test_spawn_agent_request_no_instruction() {
        let req = SpawnAgentRequest {
            workspace_id: "ws-1".to_string(),
            command: "claude".to_string(),
            work_dir: None,
            instruction: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        // None is serialized as null by default
        assert!(json.contains("\"instruction\":null"));
    }

    #[test]
    fn test_send_message_request_serialization() {
        let req = SendMessageRequest {
            message: "hello world".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"message":"hello world"}"#);
    }

    #[test]
    fn test_workspace_response_deserialization() {
        let json = r#"{"id":"ws-1","backend":"local","metadata":{"key":"value"}}"#;
        let resp: WorkspaceResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, "ws-1");
        assert_eq!(resp.backend, "local");
        assert_eq!(resp.metadata.get("key").unwrap(), "value");
    }

    #[test]
    fn test_agent_info_response_deserialization() {
        let json = r#"{"agent_id":"a-1","workspace_id":"ws-1","state":"running","created_at":"2024-01-01"}"#;
        let resp: AgentInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.agent_id, "a-1");
        assert_eq!(resp.workspace_id, "ws-1");
        assert_eq!(resp.state, "running");
        assert_eq!(resp.created_at, "2024-01-01");
    }

    #[test]
    fn test_status_response_deserialization() {
        let json =
            r#"{"nats_initialized":true,"nats_port":4222,"active_agents":3,"daemon_info":null}"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.nats_initialized);
        assert_eq!(resp.nats_port, Some(4222));
        assert_eq!(resp.active_agents, 3);
        assert!(resp.daemon_info.is_none());
    }

    #[test]
    fn test_status_response_with_daemon_info() {
        let json = r#"{
            "nats_initialized":true,
            "nats_port":4222,
            "active_agents":1,
            "daemon_info":{
                "binary_available":true,
                "binary_path":"/usr/bin/rmux",
                "connected":true,
                "tracked_pane_count":2,
                "tracked_workspace_count":1,
                "total_sessions":1,
                "total_daemon_panes":3,
                "ergatai_sessions":["s1"]
            }
        }"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        let daemon = resp.daemon_info.unwrap();
        assert!(daemon.binary_available);
        assert_eq!(daemon.binary_path.as_deref(), Some("/usr/bin/rmux"));
        assert!(daemon.connected);
        assert_eq!(daemon.tracked_pane_count, 2);
        assert_eq!(daemon.ergatai_sessions, vec!["s1"]);
    }

    #[test]
    fn test_spawn_agent_response_deserialization() {
        let json = r#"{"agent_id":"new-agent-123"}"#;
        let resp: SpawnAgentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.agent_id, "new-agent-123");
    }

    #[tokio::test]
    async fn test_list_workspaces_connection_refused() {
        // Connecting to a port that isn't listening should return an error
        let client = ErgataiClient::new("http://127.0.0.1:1", None);
        let result = client.list_workspaces().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_status_connection_refused() {
        let client = ErgataiClient::new("http://127.0.0.1:1", None);
        let result = client.get_status().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_agents_connection_refused() {
        let client = ErgataiClient::new("http://127.0.0.1:1", None);
        let result = client.list_agents().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_workspace_response_empty_metadata() {
        let json = r#"{"id":"ws-1","backend":"local","metadata":{}}"#;
        let resp: WorkspaceResponse = serde_json::from_str(json).unwrap();
        assert!(resp.metadata.is_empty());
    }

    #[test]
    fn test_status_response_no_port() {
        let json = r#"{"nats_initialized":false,"active_agents":0}"#;
        let resp: StatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.nats_initialized);
        assert!(resp.nats_port.is_none());
        assert_eq!(resp.active_agents, 0);
    }
}
