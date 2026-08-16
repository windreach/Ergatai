use crate::client::http::{AgentInfoResponse, StatusResponse, WorkspaceResponse};

pub fn format_workspaces_table(workspaces: &[WorkspaceResponse]) {
    if workspaces.is_empty() {
        println!("No workspaces found");
        return;
    }

    println!("{:<30} {:<15} {}", "ID", "BACKEND", "METADATA");
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
        "{:<40} {:<30} {:<15} {}",
        "AGENT ID", "WORKSPACE", "STATE", "CREATED"
    );
    println!("{}", "-".repeat(100));

    for a in agents {
        println!(
            "{:<40} {:<30} {:<15} {}",
            a.agent_id, a.workspace_id, a.state, a.created_at
        );
    }
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
        println!("  Total sessions: {}", daemon.total_sessions);
        println!("  Total panes: {}", daemon.total_daemon_panes);
        println!("  Ergatai sessions: {}", daemon.ergatai_sessions.len());
        for session in &daemon.ergatai_sessions {
            println!("    - {}", session);
        }
    }
}

pub fn format_json<T: serde::Serialize>(data: &T) -> Result<(), serde_json::Error> {
    let json = serde_json::to_string_pretty(data)?;
    println!("{}", json);
    Ok(())
}
