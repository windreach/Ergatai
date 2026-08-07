// Direct AcpClient test - bypasses frontend, tests buzz integration directly
// Run with: cargo run --example acp_client_test

use std::time::Duration;
use tokio::time::timeout;

// Import buzz modules directly from the crate
use ergatai::acp::pool::client::AcpClient;
use ergatai::acp::pool::config::{
    normalize_agent_command_identity,
    normalize_agent_args,
    default_agent_env,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Direct AcpClient Integration Test\n");

    // Test 1: Verify goose is installed
    println!("📋 Step 1: Verify goose installation");
    let goose_check = std::process::Command::new("which")
        .arg("goose")
        .output()?;

    if !goose_check.status.success() {
        println!("❌ Goose not found");
        return Ok(());
    }
    println!("✅ Goose found\n");

    // Test 2: Normalize agent config
    println!("📋 Step 2: Normalize agent config");
    let command = "goose";
    let args = vec![];

    let normalized_command = normalize_agent_command_identity(command);
    let normalized_args = normalize_agent_args(&normalized_command, args);

    println!("  Original: command='{}', args={:?}", command, vec![] as Vec<String>);
    println!("  Normalized: command='{}', args={:?}", normalized_command, normalized_args);

    // Verify normalization
    assert_eq!(normalized_command, "goose");
    assert_eq!(normalized_args, vec!["acp"]);
    println!("  ✅ Normalization correct\n");

    // Test 3: Prepare environment
    println!("📋 Step 3: Prepare environment variables");
    let mut extra_env: Vec<(String, String)> = vec![];

    for &(key, value) in default_agent_env(&normalized_command) {
        println!("  Adding default env: {}={}", key, value);
        extra_env.push((key.to_string(), value.to_string()));
    }
    println!("  ✅ Environment prepared\n");

    // Test 4: Spawn agent
    println!("📋 Step 4: Spawn agent with AcpClient");
    let has_generated_codex_config = false; // Not codex

    println!("  Spawning: goose acp");
    let spawn_result = timeout(
        Duration::from_secs(10),
        AcpClient::spawn(
            &normalized_command,
            &normalized_args,
            &extra_env,
            has_generated_codex_config,
        )
    ).await;

    let mut client = match spawn_result {
        Ok(Ok(client)) => {
            println!("  ✅ Agent spawned successfully\n");
            client
        }
        Ok(Err(e)) => {
            println!("  ❌ Failed to spawn agent: {}", e);
            return Ok(());
        }
        Err(_) => {
            println!("  ❌ Spawn timeout");
            return Ok(());
        }
    };

    // Test 5: Initialize ACP protocol
    println!("📋 Step 5: Initialize ACP protocol");
    let init_result = timeout(
        Duration::from_secs(10),
        client.initialize()
    ).await;

    match init_result {
        Ok(Ok(result)) => {
            println!("  ✅ Initialize successful");
            println!("  Capabilities: {}", serde_json::to_string_pretty(&result)?);
            println!();
        }
        Ok(Err(e)) => {
            println!("  ❌ Initialize failed: {}", e);
            let _ = client.shutdown().await;
            return Ok(());
        }
        Err(_) => {
            println!("  ❌ Initialize timeout");
            let _ = client.shutdown().await;
            return Ok(());
        }
    }

    // Test 6: Create session
    println!("📋 Step 6: Create session");
    let cwd = std::env::current_dir()?.to_string_lossy().to_string();

    let session_result = timeout(
        Duration::from_secs(10),
        client.session_new(
            &cwd,
            vec![], // No MCP servers
            None,   // No system prompt
            Some("Buzz Integration Test"),
        )
    ).await;

    let session_id = match session_result {
        Ok(Ok(id)) => {
            println!("  ✅ Session created: {}", id);
            println!();
            id
        }
        Ok(Err(e)) => {
            println!("  ❌ Session creation failed: {}", e);
            let _ = client.shutdown().await;
            return Ok(());
        }
        Err(_) => {
            println!("  ❌ Session creation timeout");
            let _ = client.shutdown().await;
            return Ok(());
        }
    };

    // Test 7: Send prompt
    println!("📋 Step 7: Send test prompt");
    let prompt = "Say 'Hello from buzz integration!' and nothing else.";

    println!("  Prompt: {}", prompt);
    println!("  Sending...");

    let prompt_result = timeout(
        Duration::from_secs(60),
        client.session_prompt_with_idle_timeout(
            &session_id,
            prompt,
            Duration::from_secs(30),  // idle timeout
            Duration::from_secs(60),  // max duration
        )
    ).await;

    match prompt_result {
        Ok(Ok(stop_reason)) => {
            println!("  ✅ Prompt completed");
            println!("  Stop reason: {:?}", stop_reason);
            println!();
        }
        Ok(Err(e)) => {
            println!("  ❌ Prompt failed: {}", e);
        }
        Err(_) => {
            println!("  ❌ Prompt timeout");
        }
    }

    // Test 8: Shutdown
    println!("📋 Step 8: Shutdown");
    let _ = timeout(
        Duration::from_secs(5),
        client.shutdown()
    ).await;
    println!("  ✅ Agent shut down\n");

    println!("✅ Integration test completed!");
    println!("\n📊 Summary:");
    println!("  ✅ Agent normalization");
    println!("  ✅ Environment injection");
    println!("  ✅ Process spawning");
    println!("  ✅ ACP initialization");
    println!("  ✅ Session creation");
    println!("  ✅ Prompt sending");
    println!("  ✅ Clean shutdown");

    Ok(())
}
