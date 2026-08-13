//! Agents command handlers

use crate::AgentsCommands;
use anyhow::Result;
use ergatai_core::agent::config::get_agent_config;
use ergatai_core::agent::discovery::discover_acp_runtimes;
use ergatai_core::agent::hosted_config::list_hosted_agents;

pub async fn handle(action: AgentsCommands) -> Result<()> {
    match action {
        AgentsCommands::List => {
            println!("🤖 Available agents:");

            // List hosted agents
            match list_hosted_agents() {
                Ok(names) => {
                    for name in names {
                        let (display_name, available) = match get_agent_config(&name) {
                            Ok(cfg) => (cfg.display_name, true),
                            Err(_) => (None, false),
                        };
                        let display = display_name.as_deref().unwrap_or(&name);
                        let status = if available { "✓" } else { "✗" };
                        println!("  {} {} [hosted]", status, display);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to list hosted agents: {}", e);
                }
            }

            // List built-in ACP runtimes
            for entry in discover_acp_runtimes() {
                // Skip if already listed as hosted
                if list_hosted_agents()
                    .map(|names| names.contains(&entry.id))
                    .unwrap_or(false)
                {
                    continue;
                }
                let available = matches!(
                    entry.availability,
                    ergatai_core::agent::discovery::AcpAvailabilityStatus::Available
                );
                let status = if available { "✓" } else { "✗" };
                println!("  {} {} [builtin]", status, entry.label);
            }
        }
        AgentsCommands::Info { name } => {
            println!("ℹ️  Agent info for: {}", name);
            match get_agent_config(&name) {
                Ok(cfg) => {
                    println!(
                        "  Name: {}",
                        cfg.display_name.unwrap_or_else(|| name.clone())
                    );
                    println!("  Command: {}", cfg.command);
                    if !cfg.args.is_empty() {
                        println!("  Args: {}", cfg.args.join(" "));
                    }
                }
                Err(e) => {
                    println!("  Error: {}", e);
                }
            }
        }
    }
    Ok(())
}
