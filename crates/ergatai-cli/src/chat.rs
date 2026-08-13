//! Chat module - Interactive chat interface

use anyhow::Result;

pub async fn run_chat(agent: &str, initial_message: Option<String>) -> Result<()> {
    println!("🚀 Starting chat with agent: {}", agent);

    // TODO: Initialize ACP session
    // TODO: Set up TUI interface
    // TODO: Handle message input/output

    if let Some(msg) = initial_message {
        println!("📤 Sending: {}", msg);
        // TODO: Send message to agent
    }

    println!("💬 Chat interface coming soon...");
    println!("Press Ctrl+C to exit");

    Ok(())
}
