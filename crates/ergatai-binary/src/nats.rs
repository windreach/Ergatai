//! NATS server binary locator

use std::path::PathBuf;
use ergatai_error::ErgataiResult;
use crate::finder::BinaryLocator;

static NATS_LOCATOR: BinaryLocator = BinaryLocator {
    name: "nats-server",
    env_override: Some("ERGATAI_NATS_BINARY"),
    resource_subdir_pattern: Some("nats-server-{platform}"),
};

/// Find NATS server binary using 3-layer search
pub fn find_nats_binary() -> ErgataiResult<PathBuf> {
    NATS_LOCATOR.find()
}
