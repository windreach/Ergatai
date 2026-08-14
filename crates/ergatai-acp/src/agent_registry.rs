//! Agent Registry - Track connected agents
//!
//! Maintains a list of active agents that have connected to the MCP server.
//! The registry is shared globally via `agent_registry()` to allow components
//! like AgentLauncher to look up agent endpoints.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Global agent registry instance.
///
/// This is used by components that need to look up agent information
/// (e.g., AgentLauncher looking up ACP endpoints) without having the
/// registry passed explicitly through the call chain.
static AGENT_REGISTRY: OnceLock<AgentRegistry> = OnceLock::new();

/// Get the global agent registry instance.
///
/// This is the single source of truth for tracking connected agents.
/// Created lazily on first access.
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
    pub mcp_connection_id: String,
    pub acp_connection_id: Option<String>,
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
    /// ACP HTTP endpoint for push messages (e.g., "http://localhost:8080").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acp_endpoint: Option<String>,
}

/// Agent status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Active,
    Idle,
    Disconnected,
}

/// Validate an ACP endpoint URL for security.
///
/// Returns Ok(()) if the endpoint is valid and safe (localhost-only).
/// Returns Err with a description if validation fails.
fn validate_acp_endpoint(endpoint: &str) -> Result<(), String> {
    // Parse URL
    let parsed = url::Url::parse(endpoint)
        .map_err(|e| format!("Invalid URL: {}", e))?;

    // Ensure HTTP or HTTPS scheme
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("Invalid scheme '{}': only http/https allowed", scheme)),
    }

    // Extract and validate host
    let host = parsed.host_str()
        .ok_or_else(|| "Missing host in URL".to_string())?;

    // Security: only allow localhost addresses to prevent SSRF
    let allowed_hosts = ["localhost", "127.0.0.1", "::1"];
    if !allowed_hosts.contains(&host) {
        return Err(format!(
            "Invalid host '{}': only localhost addresses allowed (localhost, 127.0.0.1, ::1)",
            host
        ));
    }

    Ok(())
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
    /// * `acp_endpoint` - Optional ACP HTTP endpoint (e.g., "http://localhost:8080")
    ///   for Ergatai to push tasks/messages to the agent. If None, Ergatai can
    ///   only respond to tool calls from this agent.
    ///
    /// # Errors
    /// Returns error if acp_endpoint is provided but fails validation (invalid URL or non-localhost).
    pub async fn register_agent(
        &self,
        agent_id: String,
        mcp_connection_id: String,
        capabilities: Option<Vec<String>>,
        acp_endpoint: Option<String>,
    ) -> Result<(), String> {
        // Validate endpoint if provided
        if let Some(ref endpoint) = acp_endpoint {
            validate_acp_endpoint(endpoint)?;
        }

        let now = Utc::now().to_rfc3339();
        let info = AgentInfo {
            agent_id: agent_id.clone(),
            status: AgentStatus::Active,
            capabilities,
            connected_at: now.clone(),
            last_heartbeat: now,
            acp_endpoint,
        };

        let record = AgentRecord {
            info,
            mcp_connection_id,
            acp_connection_id: None,
        };

        let mut agents = self.agents.write().await;
        agents.insert(agent_id, record);
        Ok(())
    }

    /// Update the ACP endpoint for an agent.
    ///
    /// This is called when an agent re-registers or updates its endpoint.
    ///
    /// # Errors
    /// Returns error if the endpoint fails validation (invalid URL or non-localhost).
    pub async fn set_acp_endpoint(
        &self,
        agent_id: &str,
        acp_endpoint: String,
    ) -> Result<(), String> {
        // Validate endpoint before storing
        validate_acp_endpoint(&acp_endpoint)?;

        let mut agents = self.agents.write().await;
        if let Some(record) = agents.get_mut(agent_id) {
            record.info.acp_endpoint = Some(acp_endpoint);
        }
        Ok(())
    }

    /// Get the ACP endpoint for an agent.
    pub async fn get_acp_endpoint(&self, agent_id: &str) -> Option<String> {
        let agents = self.agents.read().await;
        agents.get(agent_id).and_then(|r| r.info.acp_endpoint.clone())
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
