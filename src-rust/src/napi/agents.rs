//! NAPI bindings for ACP agent management.
//!
//! Exposes Rust agent management functions to TypeScript via NAPI-RS.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::agent::{
    config::{self, AgentConfig},
    custom_harness::{self, HarnessDefinition},
    discovery::{self, AcpRuntimeCatalogEntry},
    global_config::{self, GlobalAgentConfig},
    hosted_config::{self, HostedAgentConfig},
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
    global_config::load_global_agent_config().map_err(to_napi)
}

/// Save the global agent configuration.
///
/// Validates and persists the config to disk with restricted permissions (0o600).
#[napi]
pub async fn set_global_agent_config(config: GlobalAgentConfig) -> Result<()> {
    guard();
    global_config::save_global_agent_config(&config).map_err(to_napi)?;
    Ok(())
}

/// Save a custom harness definition.
///
/// Creates or updates a custom agent harness in the custom_harnesses directory.
#[napi]
pub async fn save_custom_harness(harness: HarnessDefinition) -> Result<AcpRuntimeCatalogEntry> {
    guard();
    custom_harness::save_custom_harness(&harness).map_err(to_napi)?;

    // Return the updated catalog entry for this harness
    let runtimes = discovery::discover_acp_runtimes();

    runtimes
        .into_iter()
        .find(|r| r.id == harness.id)
        .ok_or_else(|| {
            Error::from_reason(format!(
                "Failed to find harness '{}' after save",
                harness.id
            ))
        })
}

/// Delete a custom harness by id.
///
/// Removes the harness JSON file from the custom_harnesses directory.
#[napi]
pub async fn delete_custom_harness(id: String) -> Result<()> {
    guard();
    custom_harness::delete_custom_harness(&id).map_err(to_napi)
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
    config::get_agent_config(&name).map_err(to_napi)
}

/// Save agent configuration.
///
/// Creates or updates an agent config file in the agents directory.
#[napi]
pub async fn save_agent_config(cfg: AgentConfig) -> Result<()> {
    guard();
    config::save_agent_config(&cfg).map_err(to_napi)
}

// ===== Hosted Agent Configuration =====

/// NAPI-friendly representation of a hosted agent.
#[napi(object)]
pub struct NapiHostedAgent {
    /// Agent directory name (unique identifier)
    pub name: String,
    /// Full path to the agent directory
    pub dir_path: String,
    /// Underlying agent type (claude, codex, goose, hermes)
    pub agent_base: String,
    /// Display name (falls back to name if not set)
    pub display_name: String,
    /// Network proxy (optional)
    pub proxy: Option<String>,
    /// Avatar path (optional)
    pub avatar: Option<String>,
}

impl From<&HostedAgentConfig> for NapiHostedAgent {
    fn from(config: &HostedAgentConfig) -> Self {
        Self {
            name: config.name.clone(),
            dir_path: config.dir_path.to_string_lossy().to_string(),
            agent_base: config.meta.agent_base.clone(),
            display_name: hosted_config::display_name(config),
            proxy: config.meta.proxy.clone(),
            avatar: config.meta.avatar.clone(),
        }
    }
}

/// List all hosted agent names.
///
/// Returns names of agents in `~/.config/ergatai/agents/` that have a valid settings.json.
#[napi]
pub async fn list_hosted_agents() -> Result<Vec<String>> {
    guard();
    hosted_config::list_hosted_agents().map_err(to_napi)
}

/// Get detailed info about a hosted agent.
///
/// Returns the agent metadata (name, agentBase, displayName, etc.)
/// The raw agent config is not returned here — use `get_hosted_agent_settings` for that.
#[napi]
pub async fn get_hosted_agent(name: String) -> Result<NapiHostedAgent> {
    guard();
    let config = hosted_config::load_hosted_agent(&name).map_err(to_napi)?;
    Ok(NapiHostedAgent::from(&config))
}

/// Get the raw settings.json content for a hosted agent.
///
/// Returns the full JSON including the `ergatai` group.
/// This is what the UI editor should display.
#[napi]
pub async fn get_hosted_agent_settings(name: String) -> Result<String> {
    guard();
    // Validate name to prevent path traversal
    hosted_config::validate_agent_name(&name).map_err(to_napi)?;

    let base_dir = hosted_config::hosted_agents_dir().map_err(to_napi)?;
    let settings_path = base_dir.join(&name).join("settings.json");

    if !settings_path.exists() {
        return Err(Error::from_reason(format!("Agent '{}' not found", name)));
    }

    std::fs::read_to_string(&settings_path)
        .map_err(|e| Error::from_reason(format!("Failed to read settings: {}", e)))
}

/// Create a new hosted agent.
///
/// `settings_json` is the full JSON string including the `ergatai` group.
/// Returns the agent directory path on success.
#[napi]
pub async fn create_hosted_agent(name: String, settings_json: String) -> Result<String> {
    guard();
    let settings: serde_json::Value = serde_json::from_str(&settings_json)
        .map_err(|e| Error::from_reason(format!("Invalid JSON: {}", e)))?;

    let path = hosted_config::create_hosted_agent(&name, &settings).map_err(to_napi)?;

    Ok(path.to_string_lossy().to_string())
}

/// Update an existing hosted agent's settings.
///
/// `settings_json` is the full JSON string including the `ergatai` group.
#[napi]
pub async fn update_hosted_agent(name: String, settings_json: String) -> Result<()> {
    guard();
    let settings: serde_json::Value = serde_json::from_str(&settings_json)
        .map_err(|e| Error::from_reason(format!("Invalid JSON: {}", e)))?;

    hosted_config::update_hosted_agent(&name, &settings).map_err(to_napi)
}

/// Delete a hosted agent and its directory.
#[napi]
pub async fn delete_hosted_agent(name: String) -> Result<()> {
    guard();
    hosted_config::delete_hosted_agent(&name).map_err(to_napi)
}

/// Get the base directory for hosted agents.
///
/// Returns the path to `~/.config/ergatai/agents/`.
#[napi]
pub async fn get_hosted_agents_dir() -> Result<String> {
    guard();
    let dir = hosted_config::hosted_agents_dir().map_err(to_napi)?;
    Ok(dir.to_string_lossy().to_string())
}
