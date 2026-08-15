//! Agent Registry - Track connected agents
//!
//! Maintains a list of active agents that have connected to the MCP server.
//! The registry is shared globally via `agent_registry()` to allow components
//! like AgentLauncher to look up agent information.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Global agent registry instance.
///
/// This is the single source of truth for tracking connected agents.
/// Created lazily on first access.
static AGENT_REGISTRY: OnceLock<AgentRegistry> = OnceLock::new();

/// Get the global agent registry instance.
pub fn agent_registry() -> &'static AgentRegistry {
    AGENT_REGISTRY.get_or_init(AgentRegistry::new)
}

/// Agent registry - tracks all connected agents
#[derive(Clone)]
pub struct AgentRegistry {
    agents: Arc<RwLock<HashMap<String, AgentRecord>>>,
}

/// Internal agent record
#[derive(Debug, Clone)]
struct AgentRecord {
    pub info: AgentInfo,
    #[allow(dead_code)] // Reserved for future MCP connection tracking
    pub mcp_connection_id: String,
}

/// Agent information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: String,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    pub connected_at: String,
    pub last_heartbeat: String,
}

/// Agent status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Active,
    Idle,
    Disconnected,
}

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new agent.
    ///
    /// # Arguments
    /// * `agent_id` - Unique identifier for the agent
    /// * `mcp_connection_id` - The MCP connection ID (for tool calls from agent to Ergatai)
    /// * `capabilities` - List of tool names the agent provides
    pub async fn register_agent(
        &self,
        agent_id: String,
        mcp_connection_id: String,
        capabilities: Option<Vec<String>>,
    ) -> Result<(), String> {
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
        };

        let mut agents = self.agents.write().await;
        agents.insert(agent_id, record);
        Ok(())
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
