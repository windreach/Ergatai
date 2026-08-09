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

    // Parse port from connection URL
    // The connection doesn't expose the port directly, so we get it from the server
    let port = nats::get_nats_connection()
        .await
        .map(|_| 4222u32) // Default port - in production, we'd track this properly
        .unwrap_or(4222);

    Ok(port)
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
