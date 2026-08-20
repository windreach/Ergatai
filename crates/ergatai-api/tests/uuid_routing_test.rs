//! Unit tests for UUID-based message routing
//!
//! These tests verify the UUID routing logic without requiring
//! a full NATS/rmux environment.

#[cfg(test)]
mod uuid_routing_tests {
    use ergatai_runtime::backends::local_pty::LocalPtyBackend;
    use ergatai_runtime::types::{AgentHandle, AgentInfo, WorkspaceHandle};
    use ergatai_runtime::AgentRuntime;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Helper to create a test AgentHandle
    fn create_test_handle(agent_id: &str) -> AgentHandle {
        AgentHandle {
            workspace: WorkspaceHandle {
                id: format!("workspace-{}", agent_id),
                backend: "test".to_string(),
                metadata: HashMap::new(),
            },
            agent_id: agent_id.to_string(),
            process_id: Some(format!("pid-{}", agent_id)),
            metadata: HashMap::new(),
        }
    }

    /// Test: UUID resolution finds the correct agent
    #[tokio::test]
    async fn test_uuid_resolution_finds_agent() {
        let backend = Arc::new(LocalPtyBackend::new("test"));
        let runtime = AgentRuntime::new(backend);

        // Register an agent (UUID will be auto-generated)
        let agent_id = "%99";
        let handle = create_test_handle(agent_id);
        runtime
            .register_discovered_agent(agent_id.to_string(), handle)
            .await
            .unwrap();

        // Get the auto-generated UUID
        let agent_info = runtime.get_agent(agent_id).await.unwrap();
        let agent_uuid = agent_info.agent_uuid.clone();

        // Resolve UUID → should find the agent_id
        let resolved = runtime.resolve_agent_uuid(&agent_uuid).await;
        assert_eq!(resolved, Some(agent_id.to_string()));
    }

    /// Test: UUID resolution returns None for unknown UUID
    #[tokio::test]
    async fn test_uuid_resolution_returns_none_for_unknown() {
        let backend = Arc::new(LocalPtyBackend::new("test"));
        let runtime = AgentRuntime::new(backend);

        // Try to resolve a UUID that doesn't exist
        let resolved = runtime.resolve_agent_uuid("non-existent-uuid").await;
        assert_eq!(resolved, None);
    }

    /// Test: Multiple agents with different UUIDs
    #[tokio::test]
    async fn test_multiple_agents_uuid_resolution() {
        let backend = Arc::new(LocalPtyBackend::new("test"));
        let runtime = AgentRuntime::new(backend);

        // Register multiple agents
        let agent_ids = vec!["%10", "%11", "%12"];
        let mut uuid_to_id = HashMap::new();

        for id in &agent_ids {
            let handle = create_test_handle(id);
            runtime
                .register_discovered_agent(id.to_string(), handle)
                .await
                .unwrap();

            // Get the auto-generated UUID
            let agent_info = runtime.get_agent(id).await.unwrap();
            uuid_to_id.insert(agent_info.agent_uuid.clone(), id.to_string());
        }

        // Verify each UUID resolves to the correct agent_id
        for (uuid, expected_id) in &uuid_to_id {
            let resolved = runtime.resolve_agent_uuid(uuid).await;
            assert_eq!(
                resolved,
                Some(expected_id.clone()),
                "UUID {} should resolve to {}",
                uuid,
                expected_id
            );
        }
    }

    /// Test: AgentInfo has UUID field
    #[tokio::test]
    async fn test_agent_info_has_uuid_field() {
        let backend = Arc::new(LocalPtyBackend::new("test"));
        let runtime = AgentRuntime::new(backend);

        let agent_id = "%1";
        let handle = create_test_handle(agent_id);
        runtime
            .register_discovered_agent(agent_id.to_string(), handle)
            .await
            .unwrap();

        let info = runtime.get_agent(agent_id).await.unwrap();

        // UUID should be non-empty and auto-generated
        assert!(!info.agent_uuid.is_empty());
        assert_eq!(info.agent_id, agent_id);
    }

    /// Test: UUID is unique per agent
    #[tokio::test]
    async fn test_uuid_uniqueness() {
        let backend = Arc::new(LocalPtyBackend::new("test"));
        let runtime = AgentRuntime::new(backend);

        // Register multiple agents
        let agent_ids = vec!["%20", "%21", "%22", "%23"];
        let mut uuids = Vec::new();

        for id in &agent_ids {
            let handle = create_test_handle(id);
            runtime
                .register_discovered_agent(id.to_string(), handle)
                .await
                .unwrap();

            let agent_info = runtime.get_agent(id).await.unwrap();
            uuids.push(agent_info.agent_uuid.clone());
        }

        // All UUIDs should be unique
        for i in 0..uuids.len() {
            for j in (i + 1)..uuids.len() {
                assert_ne!(uuids[i], uuids[j], "UUIDs should be unique");
            }
        }
    }
}
