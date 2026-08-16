//! Simple MCP Agent Example
//!
//! Demonstrates a minimal agent that connects to Ergatai via MCP,
//! sends messages to other agents, and receives messages via
//! MCP custom notifications — no HTTP server required.
//!
//! # Usage
//!
//! ```bash
//! # Terminal 1: Start Ergatai
//! cargo run --bin ergatai-api -- --port 3000
//!
//! # Terminal 2: Start agent-alice
//! cargo run -p simple-agent -- --agent-id alice --ergatai http://localhost:3000
//!
//! # Terminal 3: Start agent-bob (will auto-message alice)
//! cargo run -p simple-agent -- --agent-id bob --ergatai http://localhost:3000
//! ```

use rmcp::{
    model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, CustomNotification, Implementation,
    },
    service::NotificationContext,
    transport::StreamableHttpClientTransport,
    ClientHandler, RoleClient, ServiceExt,
};
use tracing::{error, info};

/// The MCP client handler — receives notifications from Ergatai
struct SimpleAgent {
    agent_id: String,
}

impl SimpleAgent {
    fn new(agent_id: String) -> Self {
        Self { agent_id }
    }
}

impl ClientHandler for SimpleAgent {
    /// Override client info so Ergatai registers us under our actual agent name
    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new(self.agent_id.as_str().to_owned(), env!("CARGO_PKG_VERSION")),
        )
    }

    /// Handle custom notifications from Ergatai.
    /// This is how we receive messages from other agents.
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _ctx: NotificationContext<RoleClient>,
    ) {
        match notification.method.as_str() {
            "ergatai/message" => {
                let payload = notification.params.unwrap_or_default();
                let from = payload["from_agent"].as_str().unwrap_or("unknown");
                let content = payload["content"].as_str().unwrap_or("(empty)");
                let msg_type = payload["message_type"].as_str().unwrap_or("unknown");

                info!(
                    "📩 Received {} message from {}: {}",
                    msg_type, from, content
                );
            }
            other => {
                info!("Received notification: method={}", other);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse arguments
    let args: Vec<String> = std::env::args().collect();
    let agent_id = args
        .iter()
        .position(|a| a == "--agent-id")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "simple-agent".to_string());

    let ergatai_url = args
        .iter()
        .position(|a| a == "--ergatai")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "http://localhost:3000".to_string());

    // Init logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("🚀 Starting agent '{}' (MCP notification mode)", agent_id);
    info!("   Connecting to Ergatai at {}", ergatai_url);

    // Connect to Ergatai as MCP client
    let mcp_url = format!("{}/mcp", ergatai_url);
    let transport = StreamableHttpClientTransport::from_uri(mcp_url);

    let client_handler = SimpleAgent::new(agent_id.clone());

    let client = client_handler.serve(transport).await?;

    info!("✅ Connected to Ergatai via MCP");

    // Wait a moment for other agents to connect
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // List agents
    let list_result = client
        .call_tool(CallToolRequestParams::new("list_agents"))
        .await;

    match list_result {
        Ok(result) => {
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.as_str())
                .unwrap_or("(no output)");
            info!("📋 Connected agents: {}", text);

            // Parse agents to find someone to talk to
            if let Ok(agents_data) = serde_json::from_str::<serde_json::Value>(text) {
                let agents = agents_data["agents"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let other_agents: Vec<_> = agents
                    .iter()
                    .filter(|a| {
                        a["agent_id"]
                            .as_str()
                            .map(|id| !id.starts_with(&agent_id))
                            .unwrap_or(false)
                    })
                    .collect();

                if let Some(target) = other_agents.first() {
                    let target_id = target["agent_id"].as_str().unwrap_or("unknown");
                    let message = format!(
                        "Hello from {}! This message was delivered via MCP notification — no HTTP endpoint needed! 🎉",
                        agent_id
                    );

                    info!("💬 Sending message to {}...", target_id);

                    let mut args = serde_json::Map::new();
                    args.insert("target_agent_id".into(), serde_json::json!(target_id));
                    args.insert("message".into(), serde_json::json!(message));
                    args.insert("message_type".into(), serde_json::json!("request"));

                    match client
                        .call_tool(CallToolRequestParams::new("send_message").with_arguments(args))
                        .await
                    {
                        Ok(send_result) => {
                            let text = send_result
                                .content
                                .first()
                                .and_then(|c| c.as_text())
                                .map(|t| t.text.as_str())
                                .unwrap_or("(no output)");
                            info!("✅ {}", text);
                        }
                        Err(e) => error!("❌ send_message failed: {}", e),
                    }
                } else {
                    info!("No other agents found. Start another agent to test messaging!");
                }
            }
        }
        Err(e) => error!("❌ list_agents failed: {}", e),
    }

    // Keep running to receive notifications.
    // Send periodic heartbeats (list_agents calls) to keep the MCP session alive.
    // Without heartbeats, Ergatai's session timeout (120s default) would disconnect us.
    info!("👂 Listening for incoming MCP notifications... (Ctrl+C to exit)");

    let heartbeat_client = client.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if heartbeat_client
                .call_tool(CallToolRequestParams::new("list_agents"))
                .await
                .is_err()
            {
                break; // Server disconnected
            }
        }
    });

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    heartbeat_handle.abort();
    client.cancel().await?;
    Ok(())
}
