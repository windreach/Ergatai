//! Chat module — TUI-based interactive chat with ACP agents.

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use ergatai_core::acp::manager::{manager, SessionCommand};
use ergatai_core::acp::sdk_session::spawn_session_task;
use ergatai_core::agent::config::{get_agent_config, AgentConfig};
use ergatai_core::agent::discovery::resolve_command;
use ergatai_core::agent::runtime_metadata::KNOWN_ACP_RUNTIMES;

use crate::ui;
use crate::ui::app::AppState;

/// Resolve an agent name to an AgentConfig.
///
/// First tries `get_agent_config` (config files). If that fails, falls back
/// to discovering the agent via `KNOWN_ACP_RUNTIMES` (tries the `commands` list
/// and then `underlying_cli` fallback).
fn resolve_agent_config(name: &str) -> Result<AgentConfig> {
    // 1. Try config file
    if let Ok(config) = get_agent_config(name) {
        return Ok(config);
    }

    // 2. Check known ACP runtimes — try each command alias until one resolves
    let runtime = KNOWN_ACP_RUNTIMES
        .iter()
        .find(|r| r.id == name || r.aliases.contains(&name));

    if let Some(rt) = runtime {
        // Try commands list first (e.g. "claude-agent-acp", "claude-code-acp")
        for cmd in rt.commands {
            if resolve_command(cmd).is_some() {
                let mut env = std::collections::HashMap::new();

                // For Claude: the ACP wrapper needs CLAUDE_CODE_EXECUTABLE
                // to find the underlying Claude binary.
                if rt.id == "claude" {
                    if let Some(underlying) = rt.underlying_cli {
                        if let Some(path) = resolve_command(underlying) {
                            if std::env::var("CLAUDE_CODE_EXECUTABLE").is_err() {
                                env.insert(
                                    "CLAUDE_CODE_EXECUTABLE".to_string(),
                                    path.display().to_string(),
                                );
                            }
                        }
                    }
                }

                return Ok(AgentConfig {
                    name: name.to_string(),
                    command: cmd.to_string(),
                    args: Vec::new(),
                    env,
                    display_name: Some(rt.label.to_string()),
                    base_url: None,
                    model: None,
                    provider: None,
                    api_key: None,
                    proxy: None,
                    persona: None,
                    agent_type: Some(name.to_string()),
                    avatar: None,
                });
            }
        }
        // Fallback: try underlying_cli (e.g. "claude" for Claude Code)
        if let Some(underlying) = rt.underlying_cli {
            if resolve_command(underlying).is_some() {
                return Ok(AgentConfig {
                    name: name.to_string(),
                    command: underlying.to_string(),
                    args: Vec::new(),
                    env: std::collections::HashMap::new(),
                    display_name: Some(rt.label.to_string()),
                    base_url: None,
                    model: None,
                    provider: None,
                    api_key: None,
                    proxy: None,
                    persona: None,
                    agent_type: Some(name.to_string()),
                    avatar: None,
                });
            }
        }
    }

    anyhow::bail!(
        "Agent '{}' not found. Run `ergatai agents list` to see available agents.",
        name
    )
}

/// Run an interactive chat session with the given agent.
///
/// Creates an ACP session, then enters the full-screen ratatui TUI.
pub async fn run_chat(agent: &str, initial_message: Option<String>) -> Result<()> {
    // Resolve agent config (config file or built-in fallback)
    let config = resolve_agent_config(agent)?;

    let display_name = config
        .display_name
        .clone()
        .unwrap_or_else(|| config.name.clone());

    // Determine working directory
    let cwd = std::env::current_dir()
        .context("Failed to get current directory")?
        .to_string_lossy()
        .to_string();

    // Create ACP session BEFORE entering TUI so errors surface on the
    // normal terminal (the TUI redirects stdout into the alt screen).
    let (session_id_tx, session_id_rx) = oneshot::channel();
    spawn_session_task(config, cwd.clone(), session_id_tx);

    let session_id = session_id_rx
        .await
        .context("Session creation cancelled")?
        .context("Failed to create ACP session")?;

    // Get command sender for this session
    let cmd_tx = manager()
        .get_cmd_tx(&session_id)
        .await
        .context("Failed to get session command channel")?;

    // Build app state and enter the TUI.
    let mut app = AppState::<'static>::new(session_id.clone(), display_name.clone());
    ui::runner::run(&mut app, cmd_tx, initial_message).await
}

/// Attempt to recreate a session for the given agent (used after failed switch).
///
/// Retained for future use by the `/switch` command implementation inside the TUI.
#[allow(dead_code)]
async fn recreate_session(
    agent: &str,
    cwd: &str,
    session_id: &mut String,
    cmd_tx: &mut mpsc::UnboundedSender<SessionCommand>,
) -> Result<()> {
    let config = resolve_agent_config(agent)?;
    let (tx, rx) = oneshot::channel();
    spawn_session_task(config, cwd.to_string(), tx);

    let new_id = rx.await.context("Cancelled")?.context("Failed")?;
    let new_tx = manager()
        .get_cmd_tx(&new_id)
        .await
        .context("No cmd channel")?;

    *session_id = new_id;
    *cmd_tx = new_tx;
    Ok(())
}
