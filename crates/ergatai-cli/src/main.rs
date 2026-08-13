//! Ergatai CLI - Multi-agent collaboration from your terminal
//!
//! # Usage
//!
//! ```bash
//! # Start interactive chat
//! ergatai chat
//!
//! # Submit a DAG plan
//! ergatai dag submit plan.md
//!
//! # List available agents
//! ergatai agents list
//!
//! # Show task status
//! ergatai status
//! ```

use clap::{Parser, Subcommand};
use anyhow::Result;

mod chat;
mod commands;
mod ui;

#[derive(Parser)]
#[command(name = "ergatai")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(after_help = r#"EXAMPLES:
    # Start chat with Claude
    ergatai chat --agent claude

    # Send a one-off message
    ergatai chat -m "Help me refactor this code"

    # Submit a DAG plan
    ergatai dag submit plan.md

    # List available agents
    ergatai agents list

    # Show current status
    ergatai status

For more information, visit: https://github.com/ergatai/ergatai"#)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive chat with an agent
    Chat {
        /// Agent to chat with (default: claude)
        #[arg(short, long, default_value = "claude")]
        agent: String,

        /// Initial message to send
        #[arg(short, long)]
        message: Option<String>,
    },

    /// Manage DAG orchestration
    Dag {
        #[command(subcommand)]
        action: DagCommands,
    },

    /// Manage agents
    Agents {
        #[command(subcommand)]
        action: AgentsCommands,
    },

    /// Show current status and running tasks
    Status,
}

#[derive(Subcommand)]
enum DagCommands {
    /// Submit a DAG plan from a markdown file
    Submit {
        /// Path to the markdown file
        file: String,
    },

    /// Show DAG execution status
    Status {
        /// DAG ID (optional, shows latest if not provided)
        dag_id: Option<String>,
    },

    /// List all DAG plans
    List,
}

#[derive(Subcommand)]
enum AgentsCommands {
    /// List all available agents
    List,

    /// Show agent details
    Info {
        /// Agent name
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    if cli.verbose {
        // Safety: set_var is called before any threads are spawned
        unsafe { std::env::set_var("RUST_LOG", "debug") };
    }

    ergatai_core::init_logging();
    ergatai_core::init_panic_hook();

    // Install OS signal handlers (SIGINT/SIGTERM) so child processes
    // (NATS, MCP, ACP sessions) are cleaned up gracefully on Ctrl+C.
    if let Err(e) = ergatai_core::setup_signal_handlers().await {
        eprintln!("Warning: failed to install signal handlers: {}", e);
    }

    match cli.command {
        Commands::Chat { agent, message } => {
            chat::run_chat(&agent, message).await?;
        }
        Commands::Dag { action } => {
            commands::dag::handle(action).await?;
        }
        Commands::Agents { action } => {
            commands::agents::handle(action).await?;
        }
        Commands::Status => {
            commands::status::show().await?;
        }
    }

    Ok(())
}
