//! OS signal handling for graceful shutdown.
//!
//! Captures SIGINT (Ctrl+C) and SIGTERM, then orchestrates a graceful shutdown
//! of all subsystems (ACP sessions, agent pools, MCP servers, file access
//! control, and NATS) so that child processes are not leaked.
//!
//! # Design
//!
//! - `setup_signal_handlers()` spawns a background tokio task and returns
//!   immediately — it never blocks the caller.
//! - A second Ctrl+C during shutdown forces an immediate `process::exit(1)`,
//!   so the user is never stuck waiting for a hung shutdown.
//! - SIGTERM is Unix-only; on other platforms only SIGINT (ctrl_c) is handled.

use crate::error::ErgataiResult;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Total budget for the graceful shutdown sequence. If we exceed this, we
/// exit with a warning — individual subsystems have their own shorter
/// timeouts, so this is a safety net for unforeseen hangs.
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Install OS signal handlers that trigger a graceful shutdown.
///
/// Spawns a background tokio task; returns immediately.
pub async fn setup_signal_handlers() -> ErgataiResult<()> {
    // Shared signal counter - both tasks increment this when a signal is received
    let signal_count = Arc::new(AtomicU32::new(0));

    // Signal listener task - increments counter on each signal
    let signal_count_clone = signal_count.clone();
    tokio::spawn(async move {
        loop {
            wait_for_first_signal().await;
            let count = signal_count_clone.fetch_add(1, Ordering::SeqCst) + 1;
            tracing::info!(signal_count = count, "Signal received");
        }
    });

    // Task 1: wait for the first signal, then run graceful shutdown.
    let signal_count_clone = signal_count.clone();
    tokio::spawn(async move {
        // Wait for signal count to reach 1
        while signal_count_clone.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tracing::info!("Initiating graceful shutdown...");

        // Run shutdown under an overall timeout so a hung subsystem
        // can't block the process forever.
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, graceful_shutdown()).await {
            Ok(Ok(())) => {
                tracing::info!("Graceful shutdown completed");
                std::process::exit(0);
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Graceful shutdown failed");
                std::process::exit(1);
            }
            Err(_) => {
                tracing::error!(
                    "Graceful shutdown timed out after {:?} — forcing exit",
                    SHUTDOWN_TIMEOUT
                );
                std::process::exit(1);
            }
        }
    });

    // Task 2: a second Ctrl+C (or SIGTERM) during shutdown forces an
    // immediate exit, so the user is never stuck waiting for a hung
    // shutdown sequence.
    let signal_count_clone = signal_count.clone();
    tokio::spawn(async move {
        // Wait for signal count to reach 2
        while signal_count_clone.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tracing::warn!("Second signal received — forcing immediate exit");
        std::process::exit(1);
    });

    Ok(())
}

/// Await either SIGINT or (on Unix) SIGTERM.
async fn wait_for_first_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(e) = result {
                            tracing::error!(error = %e, "ctrl_c listener failed");
                        } else {
                            tracing::info!("Received SIGINT (Ctrl+C)");
                        }
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("Received SIGTERM");
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to install SIGTERM handler (common in containers): {}. Falling back to SIGINT only.", e);
                match tokio::signal::ctrl_c().await {
                    Ok(()) => tracing::info!("Received SIGINT (Ctrl+C)"),
                    Err(e) => tracing::error!(error = %e, "ctrl_c listener failed"),
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "ctrl_c listener failed");
        } else {
            tracing::info!("Received SIGINT (Ctrl+C)");
        }
    }
}

/// Orchestrate graceful shutdown of all subsystems.
///
/// Order matters: dependents are shut down before the services they rely on.
/// 1. Agent pools (stop event loops; they close their own sessions)
/// 2. ACP sessions (close any remaining interactive / DAG sessions)
/// 3. MCP servers (kill child processes)
/// 4. File access control (release locks, stop watchdogs)
/// 5. NATS (last — other shutdowns may publish completion events)
///
/// Each step is wrapped in its own timeout so a single stuck subsystem
/// cannot block the rest.
async fn graceful_shutdown() -> ErgataiResult<()> {
    use std::time::Duration;
    const STEP_TIMEOUT: Duration = Duration::from_secs(5);

    // 1. HTTP ACP connections (disconnect from all agents)
    tracing::info!("Step 1/5: disconnecting HTTP ACP connections...");
    match tokio::time::timeout(STEP_TIMEOUT, async {
        ergatai_acp::http_client::http_connection_manager()
            .disconnect_all()
            .await;
    })
    .await
    {
        Ok(()) => {}
        Err(_) => tracing::warn!("HTTP ACP connection shutdown timed out after {:?}", STEP_TIMEOUT),
    }

    // 2. ACP sessions
    tracing::info!("Step 2/5: closing ACP sessions...");
    match tokio::time::timeout(STEP_TIMEOUT, async {
        crate::acp::manager::manager().close_all().await;
    })
    .await
    {
        Ok(()) => {}
        Err(_) => tracing::warn!("ACP session close timed out after {:?}", STEP_TIMEOUT),
    }

    // 3. MCP servers
    // TODO(middleware): Re-enable after MCP migration
    // tracing::info!("Step 3/5: stopping MCP servers...");
    // match tokio::time::timeout(STEP_TIMEOUT, async {
    //     ergatai_acp::mcp::stop_all_mcp_servers().await;
    // })
    // .await
    // {
    //     Ok(()) => {}
    //     Err(_) => tracing::warn!("MCP server shutdown timed out after {:?}", STEP_TIMEOUT),
    // }

    // 4. File access control
    tracing::info!("Step 4/5: shutting down file access control...");
    match tokio::time::timeout(STEP_TIMEOUT, async {
        crate::file_access::manager::shutdown_all_file_access().await;
    })
    .await
    {
        Ok(()) => {}
        Err(_) => tracing::warn!("File access shutdown timed out after {:?}", STEP_TIMEOUT),
    }

    // 5. NATS (last — other steps may publish completion events)
    tracing::info!("Step 5/5: shutting down NATS...");
    match tokio::time::timeout(STEP_TIMEOUT, async {
        crate::nats::shutdown_nats().await;
    })
    .await
    {
        Ok(()) => {}
        Err(_) => tracing::warn!("NATS shutdown timed out after {:?}", STEP_TIMEOUT),
    }

    Ok(())
}
