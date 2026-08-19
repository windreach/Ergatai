//! Kill the Ergatai tmux session and clean up all agent panes.
//!
//! Usage: cargo run --bin clear-agents
//! Session name: set via ERGATAI_TMUX_SESSION env var (default: "ergatai-opencode")

use ergatai_core::tmux::TmuxManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("ergatai=info")
        .init();

    let session =
        std::env::var("ERGATAI_TMUX_SESSION").unwrap_or_else(|_| "ergatai".to_string());

    println!("Cleaning up tmux session: {}", session);

    // Check tmux is available
    if let Err(e) = TmuxManager::check_tmux().await {
        println!("tmux not available: {}", e);
        println!("Hint: install tmux or check PATH");
        return Ok(());
    }

    let manager = TmuxManager::new(&session);

    // Kill the session (best-effort; kill_session internally swallows errors
    // since a missing session is not actionable). We cannot distinguish
    // "killed" from "already gone" — both are fine for cleanup purposes.
    // NOTE: This does NOT clear the running middleware's PeerRegistry.
    // If agents were connected to ergatai-api, restart the middleware or
    // use an admin endpoint to clear stale peer registrations.
    let _ = manager.kill_session().await;
    println!("Session '{}' cleanup attempted", session);

    println!("Done.");
    Ok(())
}
