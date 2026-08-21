//! Unified Agent Registry - Single source of truth for all agent information
//!
//! This module consolidates the three separate agent registries into one:
//! - ergatai-runtime: AgentRuntime registry (tracks runtime agents)
//! - ergatai-collab: agent_launcher registry (tracks running agents in tasks)
//! - ergatai-core: agent_registry (tracks MCP-connected agents)
//!
//! The unified registry provides a consistent API for querying and managing
//! agent state across all subsystems.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use ergatai_runtime::{AgentLifecycleState, AgentRecord};

/// Global unified registry instance.
static UNIFIED_REGISTRY: OnceLock<UnifiedAgentRegistry> = OnceLock::new();

/// Get the global unified agent registry instance.
pub fn unified_registry() -> &'static UnifiedAgentRegistry {
    UNIFIED_REGISTRY.get_or_init(UnifiedAgentRegistry::new)
}

/// Unified agent registry - single source of truth for all agents
#[derive(Clone)]
pub struct UnifiedAgentRegistry {
    /// Agent records indexed by agent_uuid (stable identifier)
    agents_by_uuid: Arc<RwLock<HashMap<String, AgentRecord>>>,
    /// Index by agent_id (dynamic, e.g., pane ID like "%72")
    agent_id_to_uuid: Arc<RwLock<HashMap<String, String>>>,
    /// Index by mcp_agent_id (for MCP-connected agents)
    mcp_id_to_uuid: Arc<RwLock<HashMap<String, String>>>,
    /// Optional NATS event bus for publishing lifecycle events.
    /// Set via `set_event_bus()` during server initialization.
    event_bus: Arc<RwLock<Option<Arc<ergatai_nats::EventBus>>>>,
}

/// Summary view of an agent for API responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent_uuid: String,
    pub agent_id: String,
    pub state: String,
    pub workspace_id: String,
    pub task_id: Option<String>,
    pub mcp_agent_id: Option<String>,
    pub capabilities: Vec<String>,
    pub is_alive: bool,
    pub is_idle: bool,
    pub is_processing: bool,
    pub last_heartbeat: DateTime<Utc>,
    pub state_changed_at: DateTime<Utc>,
}

impl UnifiedAgentRegistry {
    /// Create a new empty unified registry
    pub fn new() -> Self {
        Self {
            agents_by_uuid: Arc::new(RwLock::new(HashMap::new())),
            agent_id_to_uuid: Arc::new(RwLock::new(HashMap::new())),
            mcp_id_to_uuid: Arc::new(RwLock::new(HashMap::new())),
            event_bus: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the NATS event bus for publishing lifecycle events.
    ///
    /// Called during server initialization after NATS is connected.
    /// Once set, every state transition will publish an
    /// `AgentLifecycleEventPayload` to `ergatai.agent.lifecycle.{agent_uuid}`.
    pub async fn set_event_bus(&self, bus: Arc<ergatai_nats::EventBus>) {
        *self.event_bus.write().await = Some(bus);
    }

    /// Register a new agent
    pub async fn register(&self, record: AgentRecord) {
        let uuid = record.agent_uuid.clone();
        let agent_id = record.agent_id.clone();
        let mcp_id = record.mcp_agent_id.clone();

        // Store the record
        self.agents_by_uuid
            .write()
            .await
            .insert(uuid.clone(), record);

        // Update indices
        self.agent_id_to_uuid
            .write()
            .await
            .insert(agent_id, uuid.clone());

        if let Some(mcp) = mcp_id {
            self.mcp_id_to_uuid.write().await.insert(mcp, uuid.clone());
        }

        debug!(agent_uuid = %uuid, "Agent registered in unified registry");
    }

    /// Unregister an agent by UUID
    pub async fn unregister(&self, agent_uuid: &str) -> Option<AgentRecord> {
        let record = self.agents_by_uuid.write().await.remove(agent_uuid);

        if let Some(ref r) = record {
            // Clean up indices
            self.agent_id_to_uuid.write().await.remove(&r.agent_id);

            if let Some(ref mcp) = r.mcp_agent_id {
                self.mcp_id_to_uuid.write().await.remove(mcp);
            }

            debug!(agent_uuid = %agent_uuid, "Agent unregistered from unified registry");
        }

        record
    }

    /// Get agent by UUID
    pub async fn get_by_uuid(&self, agent_uuid: &str) -> Option<AgentRecord> {
        self.agents_by_uuid.read().await.get(agent_uuid).cloned()
    }

    /// Get agent by dynamic agent_id (e.g., pane ID "%72")
    pub async fn get_by_agent_id(&self, agent_id: &str) -> Option<AgentRecord> {
        let uuid = self.agent_id_to_uuid.read().await.get(agent_id)?.clone();
        self.get_by_uuid(&uuid).await
    }

    /// Get agent by MCP agent_id (e.g., "opencode@abcd1234")
    pub async fn get_by_mcp_id(&self, mcp_agent_id: &str) -> Option<AgentRecord> {
        let uuid = self.mcp_id_to_uuid.read().await.get(mcp_agent_id)?.clone();
        self.get_by_uuid(&uuid).await
    }

    /// Update agent state with transition tracking
    ///
    /// If an event bus is configured, publishes an `AgentLifecycleEventPayload`
    /// to `ergatai.agent.lifecycle.{agent_uuid}` after the transition.
    pub async fn transition_state(
        &self,
        agent_uuid: &str,
        new_state: AgentLifecycleState,
        reason: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<(), String> {
        // Capture event data under the write lock, then release before publishing
        let event_payload = {
            let mut agents = self.agents_by_uuid.write().await;
            let record = agents
                .get_mut(agent_uuid)
                .ok_or_else(|| format!("Agent {} not found", agent_uuid))?;

            let from_state = record.state.state_name().to_string();
            let task_id = record.task_id.clone();

            record.transition_to(new_state, reason.clone(), metadata.clone());

            let to_state = record.state.state_name().to_string();
            let is_terminal = record.state.is_terminal();
            let is_alive = record.state.is_alive();
            let agent_id = record.agent_id.clone();

            debug!(agent_uuid = %agent_uuid, state = %to_state, "Agent state transitioned");

            // Build the event payload (if event bus is configured we'll publish below)
            Some(ergatai_nats::AgentLifecycleEventPayload {
                agent_uuid: agent_uuid.to_string(),
                agent_id,
                from_state,
                to_state,
                reason,
                task_id,
                is_terminal,
                is_alive,
                timestamp: Utc::now().to_rfc3339(),
                metadata,
            })
        }; // write lock released here

        // Publish lifecycle event (best-effort, don't fail the transition)
        if let Some(payload) = event_payload {
            let bus = self.event_bus.read().await.clone();
            if let Some(bus) = bus {
                if let Err(e) = bus.publish_agent_lifecycle(&payload).await {
                    warn!(
                        agent_uuid = %agent_uuid,
                        error = %e,
                        "Failed to publish agent lifecycle event"
                    );
                }
            }
        }

        Ok(())
    }

    /// Update agent heartbeat
    pub async fn update_heartbeat(&self, agent_uuid: &str) -> Result<(), String> {
        let mut agents = self.agents_by_uuid.write().await;
        let record = agents
            .get_mut(agent_uuid)
            .ok_or_else(|| format!("Agent {} not found", agent_uuid))?;

        record.update_heartbeat();
        Ok(())
    }

    /// Update MCP agent ID binding
    pub async fn set_mcp_agent_id(
        &self,
        agent_uuid: &str,
        mcp_agent_id: String,
    ) -> Result<(), String> {
        let mut agents = self.agents_by_uuid.write().await;
        let record = agents
            .get_mut(agent_uuid)
            .ok_or_else(|| format!("Agent {} not found", agent_uuid))?;

        // Remove old binding if exists
        if let Some(ref old_mcp) = record.mcp_agent_id {
            self.mcp_id_to_uuid.write().await.remove(old_mcp);
        }

        // Set new binding
        record.mcp_agent_id = Some(mcp_agent_id.clone());
        self.mcp_id_to_uuid
            .write()
            .await
            .insert(mcp_agent_id, agent_uuid.to_string());

        Ok(())
    }

    /// List all agents
    pub async fn list_all(&self) -> Vec<AgentRecord> {
        self.agents_by_uuid.read().await.values().cloned().collect()
    }

    /// List agents by state predicate
    pub async fn list_by_predicate<F>(&self, predicate: F) -> Vec<AgentRecord>
    where
        F: Fn(&AgentRecord) -> bool,
    {
        self.agents_by_uuid
            .read()
            .await
            .values()
            .filter(|r| predicate(r))
            .cloned()
            .collect()
    }

    /// List alive agents
    pub async fn list_alive(&self) -> Vec<AgentRecord> {
        self.list_by_predicate(|r| r.is_alive()).await
    }

    /// List idle agents (available for work)
    pub async fn list_idle(&self) -> Vec<AgentRecord> {
        self.list_by_predicate(|r| r.is_idle()).await
    }

    /// List agents processing tasks
    pub async fn list_processing(&self) -> Vec<AgentRecord> {
        self.list_by_predicate(|r| r.is_processing()).await
    }

    /// Get agent summary for API responses
    pub async fn get_summary(&self, agent_uuid: &str) -> Option<AgentSummary> {
        let record = self.get_by_uuid(agent_uuid).await?;
        let is_alive = record.is_alive();
        let is_idle = record.is_idle();
        let is_processing = record.is_processing();
        Some(AgentSummary {
            agent_uuid: record.agent_uuid,
            agent_id: record.agent_id,
            state: record.state.state_name().to_string(),
            workspace_id: record.workspace_id,
            task_id: record.task_id,
            mcp_agent_id: record.mcp_agent_id,
            capabilities: record.capabilities,
            is_alive,
            is_idle,
            is_processing,
            last_heartbeat: record.last_heartbeat,
            state_changed_at: record.state_changed_at,
        })
    }

    /// List all agent summaries
    pub async fn list_summaries(&self) -> Vec<AgentSummary> {
        let agents = self.list_all().await;
        agents
            .into_iter()
            .map(|r| {
                let is_alive = r.is_alive();
                let is_idle = r.is_idle();
                let is_processing = r.is_processing();
                AgentSummary {
                    agent_uuid: r.agent_uuid,
                    agent_id: r.agent_id,
                    state: r.state.state_name().to_string(),
                    workspace_id: r.workspace_id,
                    task_id: r.task_id,
                    mcp_agent_id: r.mcp_agent_id,
                    capabilities: r.capabilities,
                    is_alive,
                    is_idle,
                    is_processing,
                    last_heartbeat: r.last_heartbeat,
                    state_changed_at: r.state_changed_at,
                }
            })
            .collect()
    }

    /// Get state history for an agent
    pub async fn get_state_history(
        &self,
        agent_uuid: &str,
    ) -> Option<Vec<ergatai_runtime::StateTransition>> {
        self.get_by_uuid(agent_uuid).await.map(|r| r.state_history)
    }

    /// Clean up stale agents (no heartbeat for N seconds)
    pub async fn cleanup_stale(&self, timeout_seconds: i64) -> Vec<String> {
        let now = Utc::now();
        let mut stale_uuids = Vec::new();

        let agents = self.agents_by_uuid.read().await;
        for (uuid, record) in agents.iter() {
            let elapsed = now.signed_duration_since(record.last_heartbeat);
            if elapsed.num_seconds() >= timeout_seconds && !record.is_alive() {
                stale_uuids.push(uuid.clone());
            }
        }
        drop(agents);

        // Remove stale agents
        for uuid in &stale_uuids {
            self.unregister(uuid).await;
            warn!(agent_uuid = %uuid, "Stale agent cleaned up");
        }

        stale_uuids
    }

    /// Count agents by state category
    pub async fn count_by_state(&self) -> AgentStateCounts {
        let agents = self.agents_by_uuid.read().await;
        let mut counts = AgentStateCounts::default();

        for record in agents.values() {
            counts.total += 1;
            if record.is_alive() {
                counts.alive += 1;
            } else {
                counts.terminal += 1;
            }
            if record.is_idle() {
                counts.idle += 1;
            }
            if record.is_processing() {
                counts.processing += 1;
            }
        }

        counts
    }
}

impl Default for UnifiedAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Agent state counts for monitoring
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentStateCounts {
    pub total: usize,
    pub alive: usize,
    pub terminal: usize,
    pub idle: usize,
    pub processing: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ergatai_runtime::{
        ExitOutcome, RecordAgentHandle as AgentHandle, RecordWorkspaceHandle as WorkspaceHandle,
    };

    fn create_test_record(agent_uuid: &str, agent_id: &str) -> AgentRecord {
        AgentRecord::new(
            agent_uuid.to_string(),
            agent_id.to_string(),
            "ws-test".to_string(),
            AgentHandle {
                workspace: WorkspaceHandle {
                    id: "ws-test".to_string(),
                    backend: "test".to_string(),
                    metadata: HashMap::new(),
                },
                agent_id: agent_id.to_string(),
                process_id: Some("1234".to_string()),
                metadata: HashMap::new(),
            },
        )
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let registry = UnifiedAgentRegistry::new();
        let record = create_test_record("uuid-1", "%1");

        registry.register(record.clone()).await;

        let retrieved = registry.get_by_uuid("uuid-1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().agent_id, "%1");

        let by_agent_id = registry.get_by_agent_id("%1").await;
        assert!(by_agent_id.is_some());
        assert_eq!(by_agent_id.unwrap().agent_uuid, "uuid-1");
    }

    #[tokio::test]
    async fn test_unregister() {
        let registry = UnifiedAgentRegistry::new();
        let record = create_test_record("uuid-2", "%2");

        registry.register(record).await;
        assert!(registry.get_by_uuid("uuid-2").await.is_some());

        let removed = registry.unregister("uuid-2").await;
        assert!(removed.is_some());
        assert!(registry.get_by_uuid("uuid-2").await.is_none());
    }

    #[tokio::test]
    async fn test_mcp_id_binding() {
        let registry = UnifiedAgentRegistry::new();
        let record = create_test_record("uuid-3", "%3");

        registry.register(record).await;
        registry
            .set_mcp_agent_id("uuid-3", "opencode@abc".to_string())
            .await
            .unwrap();

        let by_mcp = registry.get_by_mcp_id("opencode@abc").await;
        assert!(by_mcp.is_some());
        assert_eq!(by_mcp.unwrap().agent_uuid, "uuid-3");
    }

    #[tokio::test]
    async fn test_list_alive() {
        let registry = UnifiedAgentRegistry::new();

        // Register two agents
        registry.register(create_test_record("uuid-5", "%5")).await;
        registry.register(create_test_record("uuid-6", "%6")).await;

        // Transition one to terminal state
        registry
            .transition_state(
                "uuid-5",
                AgentLifecycleState::Terminated {
                    outcome: ExitOutcome::Exited { exit_code: Some(0) },
                    terminated_at: Utc::now(),
                    duration_secs: 100,
                },
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let alive = registry.list_alive().await;
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].agent_uuid, "uuid-6");
    }

    #[tokio::test]
    async fn test_count_by_state() {
        let registry = UnifiedAgentRegistry::new();

        registry.register(create_test_record("uuid-7", "%7")).await;
        registry.register(create_test_record("uuid-8", "%8")).await;

        // Transition one to idle
        registry
            .transition_state(
                "uuid-7",
                AgentLifecycleState::Idle {
                    ready_since: Utc::now(),
                    capabilities: vec![],
                },
                None,
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let counts = registry.count_by_state().await;
        assert_eq!(counts.total, 2);
        assert_eq!(counts.alive, 2);
        assert_eq!(counts.idle, 1);
    }
}
