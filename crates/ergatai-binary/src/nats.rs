//! NATS server binary locator

use crate::finder::BinaryLocator;
use ergatai_error::ErgataiResult;
use std::path::PathBuf;

static NATS_LOCATOR: BinaryLocator = BinaryLocator {
    name: "nats-server",
    env_override: Some("ERGATAI_NATS_BINARY"),
    resource_subdir_pattern: Some("nats-server-{platform}"),
};

/// Find NATS server binary using 3-layer search
pub fn find_nats_binary() -> ErgataiResult<PathBuf> {
    NATS_LOCATOR.find()
}
