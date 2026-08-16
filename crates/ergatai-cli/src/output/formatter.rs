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
        "{:<40} {:<30} {:<15} CREATED",
        "AGENT ID", "WORKSPACE", "STATE"
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
