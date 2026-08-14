//! Agent Registry - Track connected agents
//!
//! Maintains a list of active agents that have connected to the MCP server.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};

use super::types::{AgentInfo, AgentStatus};

/// Agent registry - tracks all connected agents
#[derive(Clone)]
pub struct AgentRegistry {
    agents: Arc<RwLock<HashMap<String, AgentRecord>>>,
}

/// Internal agent record
#[derive(Debug, Clone)]
struct AgentRecord {
    pub info: AgentInfo,
    pub mcp_connection_id: String,
    pub acp_connection_id: Option<String>,
}

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new agent
    pub async fn register_agent(
        &self,
        agent_id: String,
        mcp_connection_id: String,
        capabilities: Option<Vec<String>>,
    ) {
        let now = Utc::now().to_rfc3339();
        let info = AgentInfo {
            agent_id: agent_id.clone(),
            status: AgentStatus::Active,
            capabilities,
            connected_at: now.clone(),
            last_heartbeat: now,
        };

        let record = AgentRecord {
            info,
            mcp_connection_id,
            acp_connection_id: None,
        };

        let mut agents = self.agents.write().await;
        agents.insert(agent_id, record);
    }

    /// Update agent heartbeat
    pub async fn update_heartbeat(&self, agent_id: &str) {
        let mut agents = self.agents.write().await;
        if let Some(record) = agents.get_mut(agent_id) {
            record.info.last_heartbeat = Utc::now().to_rfc3339();
            record.info.status = AgentStatus::Active;
        }
    }

    /// Update agent status
    pub async fn update_status(&self, agent_id: &str, status: AgentStatus) {
        let mut agents = self.agents.write().await;
        if let Some(record) = agents.get_mut(agent_id) {
            record.info.status = status;
        }
    }

    /// Set ACP connection ID for an agent
    pub async fn set_acp_connection(
        &self,
        agent_id: &str,
        acp_connection_id: String,
    ) {
        let mut agents = self.agents.write().await;
        if let Some(record) = agents.get_mut(agent_id) {
            record.acp_connection_id = Some(acp_connection_id);
        }
    }

    /// Unregister an agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        let mut agents = self.agents.write().await;
        agents.remove(agent_id);
    }

    /// Get agent info
    pub async fn get_agent(&self, agent_id: &str) -> Option<AgentInfo> {
        let agents = self.agents.read().await;
        agents.get(agent_id).map(|r| r.info.clone())
    }

    /// Get ACP connection ID for an agent
    pub async fn get_acp_connection(&self, agent_id: &str) -> Option<String> {
        let agents = self.agents.read().await;
        agents.get(agent_id).and_then(|r| r.acp_connection_id.clone())
    }

    /// List all agents
    pub async fn list_agents(&self) -> Vec<AgentInfo> {
        let agents = self.agents.read().await;
        agents.values().map(|r| r.info.clone()).collect()
    }

    /// Get active agents count
    pub async fn active_count(&self) -> usize {
        let agents = self.agents.read().await;
        agents.values().filter(|r| r.info.status == AgentStatus::Active).count()
    }

    /// Clean up stale agents (no heartbeat for N seconds)
    pub async fn cleanup_stale_agents(&self, timeout_seconds: i64) {
        let now = Utc::now();
        let mut agents = self.agents.write().await;

        agents.retain(|_, record| {
            if let Ok(last_heartbeat) = DateTime::parse_from_rfc3339(&record.info.last_heartbeat) {
                let elapsed = now.signed_duration_since(last_heartbeat.with_timezone(&Utc));
                elapsed.num_seconds() < timeout_seconds
            } else {
                true // Keep if we can't parse the timestamp
            }
        });
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
