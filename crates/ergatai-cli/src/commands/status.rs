//! Status command handler

use anyhow::Result;

pub async fn show() -> Result<()> {
    println!("📊 Ergatai Status");
    println!("=================");
    // TODO: Show active sessions, running tasks, agent status
    println!("Active sessions: 0");
    println!("Running tasks: 0");
    println!("Available agents: 3");
    Ok(())
}
