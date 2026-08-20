use crate::client::http::{AgentInfoResponse, StatusResponse, WorkspaceResponse};

pub fn format_workspaces_table(workspaces: &[WorkspaceResponse]) {
    if workspaces.is_empty() {
        println!("No workspaces found");
        return;
    }

    println!("{:<30} {:<15} METADATA", "ID", "BACKEND");
    println!("{}", "-".repeat(70));

    for w in workspaces {
        let metadata = if w.metadata.is_empty() {
            "{}".to_string()
        } else {
            format!("{:?}", w.metadata)
        };
        println!("{:<30} {:<15} {}", w.id, w.backend, metadata);
    }
}

pub fn format_agents_table(agents: &[AgentInfoResponse]) {
    if agents.is_empty() {
        println!("No agents found");
        return;
    }

    println!(
        "{:<30} {:<20} {:<15} {:<10} {:<10} LAST HEARTBEAT",
        "AGENT ID", "WORKSPACE", "STATE", "ALIVE", "TASK"
    );
    println!("{}", "-".repeat(110));

    for a in agents {
        let task_display = a.task_id.as_deref().unwrap_or("-");
        let heartbeat = if a.last_heartbeat.is_empty() {
            "-".to_string()
        } else if a.last_heartbeat.len() >= 19 && a.last_heartbeat.is_ascii() {
            // Extract time portion from RFC3339-like timestamp (bytes 11..19 = "HH:MM:SS")
            a.last_heartbeat[11..19].to_string()
        } else {
            a.last_heartbeat.clone()
        };
        let name_suffix = a
            .display_name
            .as_ref()
            .map(|n| format!(" ({})", n))
            .unwrap_or_default();

        println!(
            "{:<30} {:<20} {:<15} {:<10} {:<10} {}{}",
            a.agent_id,
            a.workspace_id,
            a.state,
            if a.is_alive { "yes" } else { "no" },
            task_display,
            heartbeat,
            name_suffix,
        );
    }

    // Summary
    let total = agents.len();
    let alive = agents.iter().filter(|a| a.is_alive).count();
    let idle = agents.iter().filter(|a| a.is_idle).count();
    let processing = agents.iter().filter(|a| a.is_processing).count();
    println!();
    println!(
        "Total: {} | Alive: {} | Idle: {} | Processing: {}",
        total, alive, idle, processing
    );
}

pub fn format_status(status: &StatusResponse) {
    println!("Ergatai System Status");
    println!("{}", "=".repeat(50));
    println!();

    println!("NATS:");
    println!("  Initialized: {}", status.nats_initialized);
    if let Some(port) = status.nats_port {
        println!("  Port: {}", port);
    }
    println!();

    println!("Agents:");
    println!("  Active: {}", status.active_agents);
    println!();

    if let Some(daemon) = &status.daemon_info {
        println!("Rmux Daemon:");
        println!("  Binary available: {}", daemon.binary_available);
        if let Some(path) = &daemon.binary_path {
            println!("  Binary path: {}", path);
        }
        println!("  Connected: {}", daemon.connected);
        println!(
            "  Tracked locally: {} pane(s), {} workspace(s)",
            daemon.tracked_pane_count, daemon.tracked_workspace_count
        );
        println!(
            "  Daemon-side totals: {} session(s), {} pane(s)",
            daemon.total_sessions, daemon.total_daemon_panes
        );
        println!("  Ergatai sessions: {}", daemon.ergatai_sessions.len());
        for session in &daemon.ergatai_sessions {
            println!("    - {}", session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::http::{AgentInfoResponse, DaemonInfo, StatusResponse, WorkspaceResponse};
    use std::collections::HashMap;

    fn make_workspace(id: &str, backend: &str) -> WorkspaceResponse {
        WorkspaceResponse {
            id: id.to_string(),
            backend: backend.to_string(),
            metadata: HashMap::new(),
        }
    }

    fn make_agent(agent_id: &str, workspace_id: &str, state: &str) -> AgentInfoResponse {
        AgentInfoResponse {
            agent_id: agent_id.to_string(),
            agent_uuid: format!("uuid-{}", agent_id),
            workspace_id: workspace_id.to_string(),
            state: state.to_string(),
            lifecycle_state: state.to_string(),
            display_name: None,
            task_id: None,
            mcp_agent_id: None,
            is_alive: state == "running" || state == "idle" || state == "processing",
            is_idle: state == "idle",
            is_processing: state == "processing",
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_heartbeat: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_format_workspaces_table_empty() {
        format_workspaces_table(&[]);
    }

    #[test]
    fn test_format_workspaces_table_single() {
        let workspaces = vec![make_workspace("ws-1", "local")];
        format_workspaces_table(&workspaces);
    }

    #[test]
    fn test_format_workspaces_table_multiple() {
        let workspaces = vec![
            make_workspace("ws-1", "local"),
            make_workspace("ws-2", "remote"),
            make_workspace("ws-3", "docker"),
        ];
        format_workspaces_table(&workspaces);
    }

    #[test]
    fn test_format_workspaces_table_with_metadata() {
        let mut ws = make_workspace("ws-meta", "local");
        ws.metadata.insert("key".to_string(), "value".to_string());
        format_workspaces_table(&[ws]);
    }

    #[test]
    fn test_format_agents_table_empty() {
        format_agents_table(&[]);
    }

    #[test]
    fn test_format_agents_table_single() {
        let agents = vec![make_agent("agent-1", "ws-1", "running")];
        format_agents_table(&agents);
    }

    #[test]
    fn test_format_agents_table_multiple() {
        let agents = vec![
            make_agent("agent-1", "ws-1", "running"),
            make_agent("agent-2", "ws-2", "stopped"),
            make_agent("agent-3", "ws-1", "pending"),
        ];
        format_agents_table(&agents);
    }

    #[test]
    fn test_format_status_basic() {
        let status = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 3,
            daemon_info: None,
        };
        format_status(&status);
    }

    #[test]
    fn test_format_status_no_nats_port() {
        let status = StatusResponse {
            nats_initialized: false,
            nats_port: None,
            active_agents: 0,
            daemon_info: None,
        };
        format_status(&status);
    }

    #[test]
    fn test_format_status_with_daemon_info() {
        let status = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 2,
            daemon_info: Some(DaemonInfo {
                binary_available: true,
                binary_path: Some("/usr/bin/rmux".to_string()),
                connected: true,
                tracked_pane_count: 3,
                tracked_workspace_count: 2,
                total_sessions: 2,
                total_daemon_panes: 5,
                ergatai_sessions: vec!["session-1".to_string(), "session-2".to_string()],
            }),
        };
        format_status(&status);
    }

    #[test]
    fn test_format_status_daemon_no_binary_path() {
        let status = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 1,
            daemon_info: Some(DaemonInfo {
                binary_available: false,
                binary_path: None,
                connected: false,
                tracked_pane_count: 0,
                tracked_workspace_count: 0,
                total_sessions: 0,
                total_daemon_panes: 0,
                ergatai_sessions: vec![],
            }),
        };
        format_status(&status);
    }

    #[test]
    fn test_format_status_daemon_empty_sessions() {
        let status = StatusResponse {
            nats_initialized: true,
            nats_port: Some(4222),
            active_agents: 0,
            daemon_info: Some(DaemonInfo {
                binary_available: true,
                binary_path: Some("/bin/rmux".to_string()),
                connected: true,
                tracked_pane_count: 1,
                tracked_workspace_count: 1,
                total_sessions: 1,
                total_daemon_panes: 1,
                ergatai_sessions: vec![],
            }),
        };
        format_status(&status);
    }

    #[test]
    fn test_workspace_metadata_empty_shows_empty_braces() {
        let ws = make_workspace("ws-empty-meta", "local");
        assert!(ws.metadata.is_empty());
        format_workspaces_table(&[ws]);
    }

    #[test]
    fn test_agent_created_at_displayed() {
        let agent = AgentInfoResponse {
            agent_id: "test-agent".to_string(),
            agent_uuid: "uuid-test".to_string(),
            workspace_id: "test-ws".to_string(),
            state: "running".to_string(),
            lifecycle_state: "running".to_string(),
            display_name: None,
            task_id: None,
            mcp_agent_id: None,
            is_alive: true,
            is_idle: false,
            is_processing: false,
            created_at: "2024-06-15T12:30:00Z".to_string(),
            last_heartbeat: "2024-06-15T12:30:00Z".to_string(),
        };
        format_agents_table(&[agent]);
    }

    #[test]
    fn test_format_workspaces_table_long_ids() {
        let workspaces = vec![make_workspace(
            "very-long-workspace-identifier-that-exceeds-column-width",
            "local-backend-type",
        )];
        format_workspaces_table(&workspaces);
    }
}
