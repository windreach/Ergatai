//! Agent Registry - Track connected agents
//!
//! Maintains a list of active agents that have connected to the MCP server.
//! The registry is shared globally via `agent_registry()` to allow components
//! like AgentLauncher to look up agent information.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

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
        agents
            .values()
            .filter(|r| r.info.status == AgentStatus::Active)
            .count()
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── AgentRegistry construction ──

    #[test]
    fn test_new_registry_is_empty() {
        let registry = AgentRegistry::new();
        // Use tokio runtime to check async methods
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(registry.list_agents().await.is_empty());
            assert_eq!(registry.active_count().await, 0);
        });
    }

    #[test]
    fn test_default_creates_empty_registry() {
        let registry = AgentRegistry::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(registry.list_agents().await.is_empty());
        });
    }

    // ── register_agent ──

    #[tokio::test]
    async fn test_register_agent_stores_info() {
        let registry = AgentRegistry::new();
        let result = registry
            .register_agent(
                "agent-1".to_string(),
                "conn-1".to_string(),
                Some(vec!["tool_a".to_string()]),
            )
            .await;
        assert!(result.is_ok());

        let agent = registry.get_agent("agent-1").await;
        assert!(agent.is_some());
        let info = agent.unwrap();
        assert_eq!(info.agent_id, "agent-1");
        assert_eq!(info.status, AgentStatus::Active);
        assert_eq!(info.capabilities, Some(vec!["tool_a".to_string()]));
    }

    #[tokio::test]
    async fn test_register_agent_without_capabilities() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-2".to_string(), "conn-2".to_string(), None)
            .await
            .unwrap();

        let info = registry.get_agent("agent-2").await.unwrap();
        assert!(info.capabilities.is_none());
    }

    #[tokio::test]
    async fn test_register_agent_sets_connected_at() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-3".to_string(), "conn-3".to_string(), None)
            .await
            .unwrap();

        let info = registry.get_agent("agent-3").await.unwrap();
        // connected_at should be a valid RFC3339 timestamp
        assert!(DateTime::parse_from_rfc3339(&info.connected_at).is_ok());
        assert!(DateTime::parse_from_rfc3339(&info.last_heartbeat).is_ok());
    }

    #[tokio::test]
    async fn test_register_multiple_agents() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a2".to_string(), "c2".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a3".to_string(), "c3".to_string(), None)
            .await
            .unwrap();

        assert_eq!(registry.list_agents().await.len(), 3);
        assert_eq!(registry.active_count().await, 3);
    }

    #[tokio::test]
    async fn test_register_agent_overwrites_existing() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-x".to_string(), "conn-old".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent(
                "agent-x".to_string(),
                "conn-new".to_string(),
                Some(vec!["new_tool".to_string()]),
            )
            .await
            .unwrap();

        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 1);
        let info = &agents[0];
        assert_eq!(info.capabilities, Some(vec!["new_tool".to_string()]));
    }

    // ── get_agent ──

    #[tokio::test]
    async fn test_get_agent_returns_none_for_missing() {
        let registry = AgentRegistry::new();
        assert!(registry.get_agent("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_get_agent_returns_clone() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-1".to_string(), "conn-1".to_string(), None)
            .await
            .unwrap();

        let a = registry.get_agent("agent-1").await.unwrap();
        let b = registry.get_agent("agent-1").await.unwrap();
        assert_eq!(a.agent_id, b.agent_id);
        // Mutating one clone doesn't affect the other
        assert_eq!(a.status, b.status);
    }

    // ── list_agents ──

    #[tokio::test]
    async fn test_list_agents_returns_all() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a2".to_string(), "c2".to_string(), None)
            .await
            .unwrap();

        let agents = registry.list_agents().await;
        assert_eq!(agents.len(), 2);
        let ids: Vec<&str> = agents.iter().map(|a| a.agent_id.as_str()).collect();
        assert!(ids.contains(&"a1"));
        assert!(ids.contains(&"a2"));
    }

    #[tokio::test]
    async fn test_list_agents_empty_registry() {
        let registry = AgentRegistry::new();
        assert!(registry.list_agents().await.is_empty());
    }

    // ── update_heartbeat ──

    #[tokio::test]
    async fn test_update_heartbeat_refreshes_timestamp() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-hb".to_string(), "conn".to_string(), None)
            .await
            .unwrap();

        let before = registry.get_agent("agent-hb").await.unwrap().last_heartbeat;
        // Small delay so timestamps differ
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        registry.update_heartbeat("agent-hb").await;
        let after = registry.get_agent("agent-hb").await.unwrap().last_heartbeat;

        assert!(after >= before);
    }

    #[tokio::test]
    async fn test_update_heartbeat_sets_status_active() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-hb2".to_string(), "conn".to_string(), None)
            .await
            .unwrap();
        registry
            .update_status("agent-hb2", AgentStatus::Idle)
            .await;
        assert_eq!(
            registry.get_agent("agent-hb2").await.unwrap().status,
            AgentStatus::Idle
        );

        registry.update_heartbeat("agent-hb2").await;
        assert_eq!(
            registry.get_agent("agent-hb2").await.unwrap().status,
            AgentStatus::Active
        );
    }

    #[tokio::test]
    async fn test_update_heartbeat_missing_agent_no_panic() {
        let registry = AgentRegistry::new();
        // Should silently do nothing for unknown agent
        registry.update_heartbeat("unknown").await;
    }

    // ── update_status ──

    #[tokio::test]
    async fn test_update_status_to_idle() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-s".to_string(), "conn".to_string(), None)
            .await
            .unwrap();
        registry
            .update_status("agent-s", AgentStatus::Idle)
            .await;

        assert_eq!(
            registry.get_agent("agent-s").await.unwrap().status,
            AgentStatus::Idle
        );
    }

    #[tokio::test]
    async fn test_update_status_to_disconnected() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-d".to_string(), "conn".to_string(), None)
            .await
            .unwrap();
        registry
            .update_status("agent-d", AgentStatus::Disconnected)
            .await;

        assert_eq!(
            registry.get_agent("agent-d").await.unwrap().status,
            AgentStatus::Disconnected
        );
    }

    #[tokio::test]
    async fn test_update_status_missing_agent_no_panic() {
        let registry = AgentRegistry::new();
        registry
            .update_status("ghost", AgentStatus::Idle)
            .await;
        // No agent added, no panic
        assert!(registry.get_agent("ghost").await.is_none());
    }

    // ── active_count ──

    #[tokio::test]
    async fn test_active_count_filters_non_active() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a-active".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a-idle".to_string(), "c2".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a-disc".to_string(), "c3".to_string(), None)
            .await
            .unwrap();

        registry
            .update_status("a-idle", AgentStatus::Idle)
            .await;
        registry
            .update_status("a-disc", AgentStatus::Disconnected)
            .await;

        assert_eq!(registry.active_count().await, 1);
    }

    #[tokio::test]
    async fn test_active_count_all_active() {
        let registry = AgentRegistry::new();
        for i in 0..5 {
            registry
                .register_agent(format!("a{}", i), format!("c{}", i), None)
                .await
                .unwrap();
        }
        assert_eq!(registry.active_count().await, 5);
    }

    #[tokio::test]
    async fn test_active_count_zero_when_all_idle() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .update_status("a1", AgentStatus::Idle)
            .await;
        assert_eq!(registry.active_count().await, 0);
    }

    // ── unregister_agent ──

    #[tokio::test]
    async fn test_unregister_removes_agent() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("agent-rm".to_string(), "conn".to_string(), None)
            .await
            .unwrap();
        assert!(registry.get_agent("agent-rm").await.is_some());

        registry.unregister_agent("agent-rm").await;
        assert!(registry.get_agent("agent-rm").await.is_none());
        assert!(registry.list_agents().await.is_empty());
    }

    #[tokio::test]
    async fn test_unregister_missing_agent_no_panic() {
        let registry = AgentRegistry::new();
        registry.unregister_agent("does-not-exist").await;
    }

    #[tokio::test]
    async fn test_unregister_updates_active_count() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a2".to_string(), "c2".to_string(), None)
            .await
            .unwrap();
        assert_eq!(registry.active_count().await, 2);

        registry.unregister_agent("a1").await;
        assert_eq!(registry.active_count().await, 1);
    }

    // ── cleanup_stale_agents ──

    #[tokio::test]
    async fn test_cleanup_stale_agents_removes_old() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("fresh".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("stale".to_string(), "c2".to_string(), None)
            .await
            .unwrap();

        // Manually backdate the stale agent's heartbeat
        {
            let mut agents = registry.agents.write().await;
            let stale = agents.get_mut("stale").unwrap();
            let old_time = Utc::now() - chrono::Duration::seconds(120);
            stale.info.last_heartbeat = old_time.to_rfc3339();
        }

        // Timeout of 60s — fresh should stay, stale should go
        registry.cleanup_stale_agents(60).await;

        assert!(registry.get_agent("fresh").await.is_some());
        assert!(registry.get_agent("stale").await.is_none());
    }

    #[tokio::test]
    async fn test_cleanup_stale_agents_keeps_all_when_fresh() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();
        registry
            .register_agent("a2".to_string(), "c2".to_string(), None)
            .await
            .unwrap();

        registry.cleanup_stale_agents(60).await;
        assert_eq!(registry.list_agents().await.len(), 2);
    }

    #[tokio::test]
    async fn test_cleanup_stale_agents_handles_invalid_timestamp() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("bad-ts".to_string(), "c1".to_string(), None)
            .await
            .unwrap();

        // Corrupt the timestamp — cleanup should retain (not panic)
        {
            let mut agents = registry.agents.write().await;
            agents.get_mut("bad-ts").unwrap().info.last_heartbeat = "not-a-timestamp".to_string();
        }

        registry.cleanup_stale_agents(60).await;
        // Kept because the timestamp couldn't be parsed
        assert!(registry.get_agent("bad-ts").await.is_some());
    }

    #[tokio::test]
    async fn test_cleanup_stale_agents_zero_timeout_removes_all() {
        let registry = AgentRegistry::new();
        registry
            .register_agent("a1".to_string(), "c1".to_string(), None)
            .await
            .unwrap();

        // Even freshly registered agents have elapsed >= 0, so timeout=0 removes all
        // (elapsed.num_seconds() < 0 is always false)
        registry.cleanup_stale_agents(0).await;
        assert!(registry.list_agents().await.is_empty());
    }

    // ── AgentInfo / AgentStatus serialization ──

    #[test]
    fn test_agent_status_serialize_lowercase() {
        let json = serde_json::to_string(&AgentStatus::Active).unwrap();
        assert_eq!(json, "\"active\"");
        let json = serde_json::to_string(&AgentStatus::Idle).unwrap();
        assert_eq!(json, "\"idle\"");
        let json = serde_json::to_string(&AgentStatus::Disconnected).unwrap();
        assert_eq!(json, "\"disconnected\"");
    }

    #[test]
    fn test_agent_status_deserialize_lowercase() {
        let status: AgentStatus = serde_json::from_str("\"active\"").unwrap();
        assert_eq!(status, AgentStatus::Active);
        let status: AgentStatus = serde_json::from_str("\"idle\"").unwrap();
        assert_eq!(status, AgentStatus::Idle);
        let status: AgentStatus = serde_json::from_str("\"disconnected\"").unwrap();
        assert_eq!(status, AgentStatus::Disconnected);
    }

    #[test]
    fn test_agent_status_equality() {
        assert_eq!(AgentStatus::Active, AgentStatus::Active);
        assert_ne!(AgentStatus::Active, AgentStatus::Idle);
        assert_ne!(AgentStatus::Idle, AgentStatus::Disconnected);
    }

    #[test]
    fn test_agent_info_serialize() {
        let info = AgentInfo {
            agent_id: "test-agent".to_string(),
            status: AgentStatus::Active,
            capabilities: Some(vec!["tool1".to_string(), "tool2".to_string()]),
            connected_at: "2024-01-01T00:00:00+00:00".to_string(),
            last_heartbeat: "2024-01-01T00:00:00+00:00".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"agent_id\":\"test-agent\""));
        assert!(json.contains("\"status\":\"active\""));
        assert!(json.contains("\"tool1\""));
    }

    #[test]
    fn test_agent_info_serialize_skips_none_capabilities() {
        let info = AgentInfo {
            agent_id: "a".to_string(),
            status: AgentStatus::Idle,
            capabilities: None,
            connected_at: "2024-01-01T00:00:00+00:00".to_string(),
            last_heartbeat: "2024-01-01T00:00:00+00:00".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("capabilities"));
    }

    #[test]
    fn test_agent_info_roundtrip() {
        let info = AgentInfo {
            agent_id: "roundtrip".to_string(),
            status: AgentStatus::Disconnected,
            capabilities: Some(vec!["x".to_string()]),
            connected_at: "2024-06-01T12:00:00+00:00".to_string(),
            last_heartbeat: "2024-06-01T12:00:00+00:00".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        let parsed: AgentInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, info.agent_id);
        assert_eq!(parsed.status, info.status);
        assert_eq!(parsed.capabilities, info.capabilities);
    }

    // ── global agent_registry() ──

    #[test]
    fn test_global_agent_registry_returns_same_instance() {
        let r1 = agent_registry() as *const AgentRegistry;
        let r2 = agent_registry() as *const AgentRegistry;
        assert_eq!(r1, r2);
    }

    // ── Clone behavior ──

    #[tokio::test]
    async fn test_clone_shares_state() {
        let registry = AgentRegistry::new();
        let clone = registry.clone();

        registry
            .register_agent("shared".to_string(), "c".to_string(), None)
            .await
            .unwrap();

        // Both see the same agent
        assert!(clone.get_agent("shared").await.is_some());
        assert_eq!(clone.list_agents().await.len(), 1);
    }
}
