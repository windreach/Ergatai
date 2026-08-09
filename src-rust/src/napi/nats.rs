//! NAPI bindings for NATS management

use napi_derive::napi;
use crate::nats;

/// Initialize NATS (start embedded nats-server + connect)
///
/// Returns the port number if successful.
/// This is idempotent - calling multiple times is safe.
#[napi]
pub async fn nats_init() -> napi::Result<u32> {
    crate::napi::guard();

    let _connection = nats::init_nats()
        .await
        .map_err(|e| napi::Error::from_reason(format!("Failed to initialize NATS: {}", e)))?;

    // Get the actual port from the running server (may differ from 4222 if it was in use)
    let port = nats::get_nats_server_port()
        .await
        .unwrap_or(4222);

    Ok(port as u32)
}

/// Check if NATS is initialized and connected
#[napi]
pub async fn nats_is_initialized() -> bool {
    crate::napi::guard();
    nats::is_nats_initialized().await
}

/// Shutdown NATS (kill server + disconnect)
#[napi]
pub async fn nats_shutdown() -> napi::Result<()> {
    crate::napi::guard();
    nats::shutdown_nats().await;
    Ok(())
}

/// Route a message from one agent to another via NATS
///
/// Enables bidirectional agent-to-agent communication.
/// The message is published to `ergatai.agent.message.{to_agent}`.
///
/// # Arguments
/// * `from_agent` - Source agent name
/// * `to_agent` - Target agent name
/// * `content` - Message content
/// * `thread_id` - Optional conversation thread ID (for multi-turn dialogs)
#[napi]
pub async fn nats_route_agent_message(
    from_agent: String,
    to_agent: String,
    content: String,
    thread_id: Option<String>,
) -> napi::Result<()> {
    crate::napi::guard();

    crate::cross_agent::message_router::route_agent_message(
        &from_agent,
        &to_agent,
        &content,
        thread_id,
    )
    .await
    .map_err(|e| napi::Error::from_reason(format!("Failed to route agent message: {}", e)))?;

    Ok(())
}

/// Scan text for @agent mentions and route messages automatically
///
/// Detects @mentions in the text (e.g., "@codex please review") and
/// automatically routes messages to the mentioned agents via NATS.
///
/// Returns the number of messages successfully routed.
///
/// # Arguments
/// * `from_agent` - Source agent name
/// * `text` - Text to scan for @mentions
/// * `thread_id` - Optional conversation thread ID
#[napi]
pub async fn nats_scan_and_route_mentions(
    from_agent: String,
    text: String,
    thread_id: Option<String>,
) -> napi::Result<u32> {
    crate::napi::guard();

    let count = crate::cross_agent::message_router::scan_and_route_mentions(
        &from_agent,
        &text,
        thread_id,
    )
    .await
    .map_err(|e| napi::Error::from_reason(format!("Failed to scan and route mentions: {}", e)))?;

    Ok(count as u32)
}
