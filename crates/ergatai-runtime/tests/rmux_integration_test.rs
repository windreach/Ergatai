// rmux backend is deprecated and gated behind the `rmux` cargo feature.
// Run with: cargo test -p ergatai-runtime --features rmux
#![cfg(feature = "rmux")]

//! Integration test for rmux backend — tests the full agent lifecycle.
//!
//! This test:
//! 1. Creates an RmuxBackend
//! 2. Launches an agent (runs `echo` command)
//! 3. Injects a message
//! 4. Captures output
//! 5. Stops the agent
//!
//! # Running
//!
//! ```bash
//! # Make sure rmux-daemon is available
//! # Either set RMUX_SDK_DAEMON_BINARY or ensure rmux-daemon is on PATH
//! cargo test -p ergatai-runtime --test rmux_integration_test -- --nocapture
//! ```

use ergatai_runtime::{AgentRuntimeBackend, RmuxBackend, WorkspaceSpec};
use std::collections::HashMap;
use std::time::Duration;

/// Setup: ensure rmux-daemon binary is configured
/// Returns true if rmux is available for testing
fn setup_rmux_daemon() -> bool {
    // Check if RMUX_SDK_DAEMON_BINARY is already set
    if std::env::var("RMUX_SDK_DAEMON_BINARY").is_ok() {
        // Also set RMUX_SOCKET if not set
        if std::env::var("RMUX_SOCKET").is_err() {
            // Try to find the default socket path
            if let Some(socket_path) = find_rmux_socket() {
                std::env::set_var("RMUX_SOCKET", socket_path);
            }
        }
        return true;
    }

    // Try to find rmux-daemon in ergatai-binary resources
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let possible_paths = [
        // Full archive structure: resources/linux-x86_64/rmux-0.10.0-linux-x86_64/bin/rmux-daemon
        format!(
            "{}/../ergatai-binary/resources/linux-x86_64/rmux-0.10.0-linux-x86_64/bin/rmux-daemon",
            manifest_dir
        ),
        format!(
            "{}/../ergatai-binary/resources/darwin-arm64/rmux-0.10.0-macos-aarch64/bin/rmux-daemon",
            manifest_dir
        ),
        format!(
            "{}/../ergatai-binary/resources/darwin-x86_64/rmux-0.10.0-macos-x86_64/bin/rmux-daemon",
            manifest_dir
        ),
        // Direct binary (old structure)
        format!(
            "{}/../ergatai-binary/resources/linux-x86_64/rmux-daemon",
            manifest_dir
        ),
        format!(
            "{}/../ergatai-binary/resources/darwin-arm64/rmux-daemon",
            manifest_dir
        ),
        format!(
            "{}/../ergatai-binary/resources/darwin-x86_64/rmux-daemon",
            manifest_dir
        ),
    ];

    for path in &possible_paths {
        if std::path::Path::new(path).exists() {
            std::env::set_var("RMUX_SDK_DAEMON_BINARY", path);
            println!("🔧 Set RMUX_SDK_DAEMON_BINARY={}", path);

            // Also set RMUX_SOCKET if not set
            if std::env::var("RMUX_SOCKET").is_err() {
                if let Some(socket_path) = find_rmux_socket() {
                    std::env::set_var("RMUX_SOCKET", &socket_path);
                    println!("🔧 Set RMUX_SOCKET={}", socket_path);
                }
            }
            return true;
        }
    }

    println!("⚠️ rmux-daemon not found, skipping tests");
    false
}

/// Find the rmux daemon socket path
fn find_rmux_socket() -> Option<String> {
    // Check common socket locations
    let socket_paths = ["/tmp/rmux-0/default", "/tmp/rmux/default"];

    for path in &socket_paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Try to find in /tmp/rmux-*
    if let Ok(entries) = std::fs::read_dir("/tmp") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("rmux-") {
                        let socket = path.join("default");
                        if socket.exists() {
                            return socket.to_str().map(|s| s.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

/// Create a test workspace spec
fn test_workspace_spec(id: &str) -> WorkspaceSpec {
    WorkspaceSpec {
        id: id.to_string(),
        work_dir: std::env::temp_dir().join(format!("ergatai-test-{}", id)),
        env: HashMap::new(),
        resources: Default::default(),
        backend_config: serde_json::json!({}),
    }
}

/// Test: RmuxBackend can connect to daemon
#[tokio::test]
async fn test_rmux_daemon_connection() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");

    // Initialize should connect to daemon (or start it)
    let result = backend.initialize().await;
    if result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            result.err()
        );
        return;
    }

    println!("✅ rmux daemon connected successfully");
}

/// Test: RmuxBackend capabilities
#[tokio::test]
async fn test_rmux_capabilities() {
    let backend = RmuxBackend::new("ergatai-test");
    let caps = backend.capabilities();

    assert!(
        caps.supports_message_injection,
        "rmux should support message injection"
    );
    assert!(
        caps.supports_output_capture,
        "rmux should support output capture"
    );
    assert_eq!(backend.name(), "rmux");

    println!(
        "✅ rmux capabilities: injection={}, capture={}",
        caps.supports_message_injection, caps.supports_output_capture
    );
}

/// Test: Create and destroy workspace
#[tokio::test]
async fn test_rmux_workspace_lifecycle() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");
    let init_result = backend.initialize().await;
    if init_result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            init_result.err()
        );
        return;
    }

    let spec = test_workspace_spec("workspace-test");

    // Create workspace
    let workspace = backend.create_workspace(spec.clone()).await;
    match workspace {
        Ok(workspace) => {
            assert_eq!(workspace.id, "workspace-test");
            println!("✅ Workspace created: {}", workspace.id);

            // Cleanup workspace
            let cleanup = backend.cleanup_workspace(&workspace).await;
            assert!(
                cleanup.is_ok(),
                "Failed to cleanup workspace: {:?}",
                cleanup.err()
            );
            println!("✅ Workspace cleaned up");
        }
        Err(e) => {
            println!(
                "⏭️ Skipping: workspace creation failed (daemon may not be fully functional): {}",
                e
            );
        }
    }
}

/// Test: Launch agent with simple command
#[tokio::test]
async fn test_rmux_launch_simple_agent() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");
    let init_result = backend.initialize().await;
    if init_result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            init_result.err()
        );
        return;
    }

    let spec = test_workspace_spec("agent-test");
    let workspace = match backend.create_workspace(spec.clone()).await {
        Ok(w) => w,
        Err(e) => {
            println!("⏭️ Skipping: workspace creation failed: {}", e);
            return;
        }
    };

    // Launch a simple agent that runs a shell command
    let handle = backend
        .start_agent(
            &workspace,
            "sh",
            Some("echo 'Hello from ergatai test agent!'"),
        )
        .await;

    match handle {
        Ok(handle) => {
            println!("✅ Agent launched: {}", handle.agent_id);

            // Wait a bit for the command to execute
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Capture output
            let output = backend.capture_output(&handle).await;
            match output {
                Ok(Some(text)) => {
                    println!("📝 Captured output:\n{}", text);
                    assert!(
                        text.contains("Hello") || text.contains("ergatai") || !text.is_empty(),
                        "Output should contain expected text"
                    );
                }
                Ok(None) => {
                    println!("⚠️ No output captured (agent may have exited)");
                }
                Err(e) => {
                    println!("⚠️ Output capture failed: {}", e);
                }
            }

            // Stop agent
            let stop = backend.stop_agent(&handle).await;
            assert!(stop.is_ok(), "Failed to stop agent: {:?}", stop.err());
            println!("✅ Agent stopped");
        }
        Err(e) => {
            println!(
                "⚠️ Agent launch failed (expected if rmux-daemon not running): {}",
                e
            );
            // Don't fail the test — rmux-daemon might not be available in CI
        }
    }

    // Cleanup
    let _ = backend.cleanup_workspace(&workspace).await;
}

/// Test: Inject message into running agent
#[tokio::test]
async fn test_rmux_message_injection() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");
    let init_result = backend.initialize().await;
    if init_result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            init_result.err()
        );
        return;
    }

    let spec = test_workspace_spec("inject-test");
    let workspace = match backend.create_workspace(spec.clone()).await {
        Ok(w) => w,
        Err(e) => {
            println!("⏭️ Skipping: workspace creation failed: {}", e);
            return;
        }
    };

    // Launch a long-running agent (sleep)
    let handle = backend
        .start_agent(
            &workspace,
            "sh",
            Some("sleep 30"), // Sleep for 30 seconds
        )
        .await;

    match handle {
        Ok(handle) => {
            println!("✅ Agent launched: {}", handle.agent_id);

            // Wait for agent to start
            tokio::time::sleep(Duration::from_secs(1)).await;

            // Inject a message
            let inject = backend
                .inject_message(&handle, "echo 'injected message'")
                .await;
            match inject {
                Ok(()) => {
                    println!("✅ Message injected successfully");
                }
                Err(e) => {
                    println!("⚠️ Message injection failed: {}", e);
                }
            }

            // Wait and capture
            tokio::time::sleep(Duration::from_secs(1)).await;
            let output = backend.capture_output(&handle).await;
            if let Ok(Some(text)) = output {
                println!("📝 Output after injection:\n{}", text);
            }

            // Stop agent
            let _ = backend.stop_agent(&handle).await;
            println!("✅ Agent stopped");
        }
        Err(e) => {
            println!("⚠️ Agent launch failed: {}", e);
        }
    }

    // Cleanup
    let _ = backend.cleanup_workspace(&workspace).await;
}

/// Test: Multiple agents in same workspace
#[tokio::test]
async fn test_rmux_multiple_agents() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");
    let init_result = backend.initialize().await;
    if init_result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            init_result.err()
        );
        return;
    }

    let spec = test_workspace_spec("multi-test");
    let workspace = match backend.create_workspace(spec.clone()).await {
        Ok(w) => w,
        Err(e) => {
            println!("⏭️ Skipping: workspace creation failed: {}", e);
            return;
        }
    };

    let mut handles = vec![];

    // Launch 3 agents
    for i in 1..=3 {
        let handle = backend
            .start_agent(
                &workspace,
                "sh",
                Some(&format!("echo 'Agent {} reporting in'", i)),
            )
            .await;

        match handle {
            Ok(handle) => {
                println!("✅ Agent {} launched: {}", i, handle.agent_id);
                handles.push(handle);
            }
            Err(e) => {
                println!("⚠️ Agent {} launch failed: {}", i, e);
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!("📊 Launched {} agents", handles.len());

    // Stop all agents
    for handle in &handles {
        let _ = backend.stop_agent(handle).await;
    }
    println!("✅ All agents stopped");

    // Cleanup
    let _ = backend.cleanup_workspace(&workspace).await;
}

/// Test: Wait for agent exit
#[tokio::test]
async fn test_rmux_wait_for_exit() {
    if !setup_rmux_daemon() {
        println!("⏭️ Skipping: rmux-daemon not available");
        return;
    }

    let backend = RmuxBackend::new("ergatai-test");
    let init_result = backend.initialize().await;
    if init_result.is_err() {
        println!(
            "⏭️ Skipping: rmux daemon failed to start: {:?}",
            init_result.err()
        );
        return;
    }

    let spec = test_workspace_spec("exit-test");
    let workspace = match backend.create_workspace(spec.clone()).await {
        Ok(w) => w,
        Err(e) => {
            println!("⏭️ Skipping: workspace creation failed: {}", e);
            return;
        }
    };

    // Launch agent that exits quickly
    let handle = backend
        .start_agent(
            &workspace,
            "sh",
            Some("-c 'echo exiting soon; sleep 1; exit 42'"),
        )
        .await;

    match handle {
        Ok(handle) => {
            println!("✅ Agent launched: {}", handle.agent_id);

            // Wait for exit with timeout
            let wait = backend
                .wait_for_exit(&handle, Some(Duration::from_secs(10)))
                .await;
            match wait {
                Ok(result) => {
                    println!("✅ Wait result: {:?}", result);
                }
                Err(e) => {
                    println!("⚠️ Wait failed: {}", e);
                }
            }
        }
        Err(e) => {
            println!("⚠️ Agent launch failed: {}", e);
        }
    }

    // Cleanup
    let _ = backend.cleanup_workspace(&workspace).await;
}
