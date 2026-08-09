//! NAPI bindings for ACP agent management.
//!
//! Exposes Rust agent management functions to TypeScript via NAPI-RS.

use napi_derive::napi;
use napi::bindgen_prelude::*;

use crate::agent::{
    config::{self, AgentConfig},
    custom_harness::{self, HarnessDefinition},
    discovery::{self, AcpRuntimeCatalogEntry},
    global_config::{self, GlobalAgentConfig},
    install::{self},
};

use super::{guard, to_napi};

/// Discover all available ACP runtimes and their status.
///
/// Returns a list of catalog entries for both builtin and custom agents.
#[napi]
pub async fn discover_acp_runtimes() -> Result<Vec<AcpRuntimeCatalogEntry>> {
    guard();
    Ok(discovery::discover_acp_runtimes())
}

/// Load the global agent configuration.
///
/// Returns the current global config (env_vars, provider, model, preferred_runtime).
#[napi]
pub async fn get_global_agent_config() -> Result<GlobalAgentConfig> {
    guard();
    global_config::load_global_agent_config()
        .map_err(to_napi)
}

/// Save the global agent configuration.
///
/// Validates and persists the config to disk with restricted permissions (0o600).
#[napi]
pub async fn set_global_agent_config(config: GlobalAgentConfig) -> Result<()> {
    guard();
    global_config::save_global_agent_config(&config)
        .map_err(to_napi)?;
    Ok(())
}

/// Save a custom harness definition.
///
/// Creates or updates a custom agent harness in the custom_harnesses directory.
#[napi]
pub async fn save_custom_harness(harness: HarnessDefinition) -> Result<AcpRuntimeCatalogEntry> {
    guard();
    custom_harness::save_custom_harness(&harness)
        .map_err(to_napi)?;

    // Return the updated catalog entry for this harness
    let runtimes = discovery::discover_acp_runtimes();

    runtimes
        .into_iter()
        .find(|r| r.id == harness.id)
        .ok_or_else(|| Error::from_reason(format!(
            "Failed to find harness '{}' after save",
            harness.id
        )))
}

/// Delete a custom harness by id.
///
/// Removes the harness JSON file from the custom_harnesses directory.
#[napi]
pub async fn delete_custom_harness(id: String) -> Result<()> {
    guard();
    custom_harness::delete_custom_harness(&id)
        .map_err(to_napi)
}

/// Install an ACP runtime by executing its predefined install command.
///
/// Runs the install command (e.g., `npm install -g @block/goose`) via shell.
/// The command must be in the whitelist to prevent injection attacks.
/// Returns the stdout output on success.
#[napi]
pub async fn install_acp_runtime(runtime_id: String) -> Result<String> {
    guard();
    install::install_acp_runtime(&runtime_id)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// Get agent configuration by name.
///
/// Returns the full agent config (command, args, env, model, etc.)
#[napi]
pub async fn get_agent_config(name: String) -> Result<AgentConfig> {
    guard();
    config::get_agent_config(&name)
        .map_err(to_napi)
}

/// Save agent configuration.
///
/// Creates or updates an agent config file in the agents directory.
#[napi]
pub async fn save_agent_config(cfg: AgentConfig) -> Result<()> {
    guard();
    config::save_agent_config(&cfg)
        .map_err(to_napi)
}
