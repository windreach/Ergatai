//! Scan tmux session for running panes and register them as agents.
//!
//! Usage: cargo run --bin scan-tmux-agents
//! Session name: set via ERGATAI_TMUX_SESSION env var (default: "ergatai-opencode")

use ergatai_core::tmux::TmuxManager;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("ergatai=info")
        .init();

    println!("Scanning tmux session for agents");
    println!("=================================\n");

    // Create tmux manager — session name from env or default
    let session = std::env::var("ERGATAI_TMUX_SESSION")
        .unwrap_or_else(|_| "ergatai-opencode".to_string());
    let manager = Arc::new(TmuxManager::new(&session));

    // Check tmux availability
    println!("1. Checking tmux...");
    TmuxManager::check_tmux().await?;
    println!("   tmux is available\n");

    // Scan panes
    println!("2. Scanning tmux session '{}'...", session);
    let registered = manager.scan_and_register_panes().await?;

    if registered.is_empty() {
        println!("   No panes found");
        println!("\nHint:");
        println!("   1. Ensure tmux session '{}' exists", session);
        println!("   2. Or run: ./test-opencode-collaboration.sh");
        return Ok(());
    }

    println!("   Found and registered {} agents\n", registered.len());

    // List all agents
    println!("3. Registered agents:");
    let agents = manager.list_agents().await;
    for agent in agents {
        println!("   - {} (pane: {}, command: {})",
                 agent.agent_id, agent.pane, agent.command);
    }
    println!();

    println!("Scan complete.");
    println!("\nThese agents can now communicate via Ergatai:");
    println!("   - Agents call MCP tool send_message to send messages");
    println!("   - Ergatai injects messages into target agent's tmux pane");

    Ok(())
}
