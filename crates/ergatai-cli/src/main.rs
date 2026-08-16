use clap::{Parser, Subcommand};
use anyhow::Result;

mod commands;
mod client;
mod output;

#[derive(Parser)]
#[command(name = "ergatai")]
#[command(about = "Ergatai CLI - Multi-agent collaboration middleware")]
#[command(version)]
struct Cli {
    /// API server URL
    #[arg(long, default_value = "http://localhost:3000", env = "ERGATAI_API_URL")]
    api_url: String,

    /// API token for authentication
    #[arg(long, env = "ERGATAI_API_TOKEN")]
    token: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage workspaces
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    /// Manage agents
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Show system status
    Status {
        /// Watch for real-time updates via WebSocket
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// List all workspaces
    List,
    /// Create a new workspace
    Create {
        /// Workspace ID
        id: String,
        /// Working directory
        #[arg(long)]
        work_dir: Option<String>,
    },
    /// Delete a workspace
    Delete {
        /// Workspace ID
        id: String,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// List all agents
    List,
    /// Spawn a new agent
    Spawn {
        /// Workspace ID
        workspace: String,
        /// Command to run
        #[arg(long)]
        command: String,
        /// Initial instruction
        #[arg(long)]
        instruction: Option<String>,
    },
    /// Stop an agent
    Kill {
        /// Agent ID
        id: String,
    },
    /// Send message to an agent
    Message {
        /// Agent ID
        id: String,
        /// Message text
        message: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Workspace { action } => {
            commands::workspace::handle(action, &cli.api_url, cli.token.as_deref()).await?;
        }
        Commands::Agent { action } => {
            commands::agent::handle(action, &cli.api_url, cli.token.as_deref()).await?;
        }
        Commands::Status { watch } => {
            commands::status::handle(watch, &cli.api_url, cli.token.as_deref()).await?;
        }
    }

    Ok(())
}
