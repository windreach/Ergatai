//! rmux daemon binary locator and configuration

use std::path::PathBuf;
use ergatai_error::ErgataiResult;
use crate::finder::BinaryLocator;

static RMUX_LOCATOR: BinaryLocator = BinaryLocator {
    name: "rmux-daemon",
    env_override: Some("ERGATAI_RMUX_BINARY"),
    resource_subdir_pattern: Some("rmux-daemon-{platform}"),
};

/// Find rmux-daemon binary and set RMUX_SDK_DAEMON_BINARY environment variable
pub fn configure_rmux_daemon() -> ErgataiResult<PathBuf> {
    let path = RMUX_LOCATOR.find()?;

    // Set environment variable for rmux-sdk to discover
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &path);

    tracing::info!(
        path = %path.display(),
        "rmux-daemon binary located, RMUX_SDK_DAEMON_BINARY set"
    );

    Ok(path)
}

/// Check if rmux-daemon is available (without setting env var)
pub fn is_rmux_available() -> bool {
    RMUX_LOCATOR.find().is_ok()
}
