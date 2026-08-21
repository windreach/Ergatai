use crate::client::http::ErgataiClient;
use anyhow::{Context, Result};
use std::process::Command;

/// Sanitize a string to be a valid workspace_id.
/// Converts paths like "./start-opencode-1.sh" to "start-opencode-1"
fn sanitize_workspace_id(name: &str) -> String {
    name
        // Remove leading "./" or "/"
        .trim_start_matches("./")
        .trim_start_matches('/')
        // Remove file extension
        .rsplit_once('.')
        .map(|(base, ext)| {
            // Only remove if extension looks like a file extension (short, no slashes)
            if ext.len() <= 10 && !ext.contains('/') {
                base
            } else {
                name
            }
        })
        .unwrap_or(name)
        // Replace any remaining invalid chars with hyphens
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        // Remove leading/trailing hyphens
        .trim_matches('-')
        .to_string()
}

pub async fn handle(
    agent_name: &str,
    work_dir: Option<&str>,
    api_url: &str,
    token: Option<&str>,
    persist: bool,
) -> Result<()> {
    let client = ErgataiClient::new(api_url, token);

    // Resolve agent_name to absolute path once, then derive both work_dir and command.
    let path = std::path::Path::new(agent_name);
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        cwd.join(path)
    };

    // Use provided work_dir, or derive from agent_name if it's a path, or current directory
    let effective_work_dir = match work_dir {
        Some(dir) => Some(dir.to_string()),
        None => {
            // Canonicalize to resolve symlinks and ".." components
            let canonical = std::fs::canonicalize(&abs_path).unwrap_or_else(|_| abs_path.clone());
            let parent = canonical.parent();
            match parent {
                Some(p) if !p.as_os_str().is_empty() => Some(p.to_string_lossy().to_string()),
                _ => {
                    // Fallback to current directory
                    let cwd = std::env::current_dir().context("Failed to get current directory")?;
                    Some(cwd.to_string_lossy().to_string())
                }
            }
        }
    };

    // Sanitize workspace_id (convert "./start-opencode-1.sh" to "start-opencode-1")
    let workspace_id = sanitize_workspace_id(agent_name);

    // Convert agent_name to absolute path if it looks like a path, otherwise pass through
    let agent_command = if path.is_absolute() {
        agent_name.to_string()
    } else if agent_name.contains('/') || agent_name.contains('.') {
        // Looks like a path (has separators or dots) — use the resolved absolute path
        abs_path.to_string_lossy().to_string()
    } else {
        // Just a command name (e.g., "claude", "opencode") — pass through
        // as-is and let the shell find it in PATH
        agent_name.to_string()
    };

    // Step 1: Create workspace (reuse if exists)
    println!("🚀 Starting workspace: {}", agent_name);
    println!(
        "📁 Working directory: {}",
        effective_work_dir.as_deref().unwrap_or("<default>")
    );
    let workspace = client
        .create_workspace(&workspace_id, effective_work_dir.as_deref(), persist)
        .await
        .context("Failed to create workspace")?;
    println!("✓ Workspace ready: {} (id: {})", agent_name, workspace.id);

    // Step 2: Check if agent already running in this workspace
    let agents = client.list_agents().await?;
    let existing_agent = agents.iter().find(|a| {
        a.workspace_id == workspace_id
            // Prefer the is_alive boolean from the unified lifecycle state machine.
            // Fall back to case-insensitive string comparison for backward compat.
            && (a.is_alive || a.state.eq_ignore_ascii_case("running"))
    });

    if let Some(agent) = existing_agent {
        println!("✓ Agent already running: {}", agent.agent_id);
    } else {
        // Step 3: Spawn agent (use absolute path as command, pass work_dir)
        println!("✓ Spawning agent: {}", agent_command);
        let response = client
            .spawn_agent(
                &workspace_id,
                &agent_command,
                effective_work_dir.as_deref(),
                None,
            )
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

    // Call tmux attach
    let status = Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()
        .context("Failed to execute 'tmux attach-session'. Is tmux installed?")?;

    if !status.success() {
        anyhow::bail!("tmux attach-session exited with status: {}", status);
    }

    Ok(())
}
