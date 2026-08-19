use crate::client::http::ErgataiClient;
use anyhow::{Context, Result};
use std::process::Command;

pub async fn handle(
    agent_name: &str,
    work_dir: Option<&str>,
    api_url: &str,
    token: Option<&str>,
) -> Result<()> {
    let client = ErgataiClient::new(api_url, token);

    // Use provided work_dir, or current directory
    let effective_work_dir = match work_dir {
        Some(dir) => Some(dir.to_string()),
        None => {
            let cwd = std::env::current_dir().context("Failed to get current directory")?;
            Some(cwd.to_string_lossy().to_string())
        }
    };

    // Step 1: Create workspace (reuse if exists)
    println!("🚀 Starting workspace: {}", agent_name);
    println!(
        "📁 Working directory: {}",
        effective_work_dir.as_deref().unwrap_or("<default>")
    );
    let workspace = client
        .create_workspace(agent_name, effective_work_dir.as_deref())
        .await
        .context("Failed to create workspace")?;
    println!("✓ Workspace ready: {}", workspace.id);

    // Step 2: Check if agent already running in this workspace
    let agents = client.list_agents().await?;
    let existing_agent = agents
        .iter()
        .find(|a| a.workspace_id == agent_name && a.state == "running");

    if let Some(agent) = existing_agent {
        println!("✓ Agent already running: {}", agent.agent_id);
    } else {
        // Step 3: Spawn agent
        println!("✓ Spawning agent: {}", agent_name);
        let response = client
            .spawn_agent(agent_name, agent_name, None)
            .await
            .context("Failed to spawn agent")?;
        println!("✓ Agent spawned: {}", response.agent_id);
    };

    // Step 4: Attach to session
    let session_name = workspace
        .metadata
        .get("session")
        .context("Workspace metadata missing 'session' key")?;

    println!("✓ Attaching to session: {}", session_name);
    println!("   (Press Ctrl+B, D to detach)");
    println!();

    // Call rmux attach
    let status = Command::new("rmux")
        .args(["attach", "-t", session_name])
        .status()
        .context("Failed to execute 'rmux attach'. Is rmux installed?")?;

    if !status.success() {
        anyhow::bail!("rmux attach exited with status: {}", status);
    }

    Ok(())
}
