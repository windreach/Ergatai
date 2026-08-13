//! Global agent configuration management.
//!
//! A unified configuration model that applies to all agents.
//! Stored in `<app-data>/agents/global-agent-config.json` with restricted permissions.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ErgataiError};

/// Global agent configuration record.
///
/// Applies to ALL managed agents. Precedence (low → high):
/// GLOBAL < per-agent config < runtime metadata injection
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlobalAgentConfig {
    /// Global env vars injected into ALL agents unconditionally.
    ///
    /// Lowest user-settable layer — per-agent values win on any key collision.
    #[serde(default)]
    pub env_vars: BTreeMap<String, String>,

    /// Global fallback provider (e.g., "anthropic", "openai").
    ///
    /// Used when neither the agent record nor runtime specifies a provider.
    #[serde(default)]
    pub provider: Option<String>,

    /// Global fallback model identifier.
    ///
    /// Used when neither the agent record nor runtime specifies a model.
    #[serde(default)]
    pub model: Option<String>,

    /// Preferred ACP runtime for new agents without an explicit runtime.
    #[serde(default)]
    pub preferred_runtime: Option<String>,
}

/// Result of saving global config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalAgentConfigSaveResult {
    pub config: GlobalAgentConfig,
}

/// Derived provider/model env keys that must NOT be set as global env vars.
///
/// These are derived from the structured `provider`/`model` fields at spawn time.
/// Users must use the structured fields instead.
pub const DERIVED_PROVIDER_MODEL_ENV_KEYS: &[&str] = &[
    "GOOSE_MODEL",
    "GOOSE_PROVIDER",
    "BUZZ_AGENT_MODEL",
    "BUZZ_AGENT_PROVIDER",
];

/// Maximum allowed size for an env var value (in bytes).
pub const MAX_ENV_VALUE_BYTES: usize = 8192;

/// Get the path to the global config file.
fn global_config_path() -> Result<PathBuf, ErgataiError> {
    let app_data = dirs::config_dir()
        .ok_or(ErgataiError::ConfigError(ConfigError::DirectoryNotFound))?
        .join("ergatai")
        .join("agents");

    fs::create_dir_all(&app_data)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

    Ok(app_data.join("global-agent-config.json"))
}

/// Load the global agent configuration from disk.
///
/// Returns a default (empty) config if the file doesn't exist.
pub fn load_global_agent_config() -> Result<GlobalAgentConfig, ErgataiError> {
    let path = global_config_path()?;

    if !path.exists() {
        return Ok(GlobalAgentConfig::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

    let config: GlobalAgentConfig = serde_json::from_str(&content)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ParseFailed { source: e }))?;

    Ok(config)
}

/// Validate a global agent configuration before persisting.
///
/// Rules:
/// - Empty env values are stripped
/// - Reserved keys (DERIVED_PROVIDER_MODEL_ENV_KEYS) are rejected in env_vars
/// - NUL bytes are rejected
/// - Values exceeding MAX_ENV_VALUE_BYTES are rejected
pub fn validate_global_config(config: &GlobalAgentConfig) -> Result<(), ErgataiError> {
    // Strip empty values first
    let non_empty: BTreeMap<String, String> = config
        .env_vars
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Check for derived keys
    let derived: Vec<&str> = non_empty
        .keys()
        .filter(|k| {
            DERIVED_PROVIDER_MODEL_ENV_KEYS
                .iter()
                .any(|d| d.eq_ignore_ascii_case(k.as_str()))
        })
        .map(String::as_str)
        .collect();

    if !derived.is_empty() {
        return Err(ErgataiError::InvalidArgument(format!(
            "the following keys must be set via the structured provider/model fields, not as env vars: {}",
            derived.join(", ")
        )));
    }

    // Validate env var keys and values
    for (key, value) in &non_empty {
        // Check for well-formed key
        if !is_well_formed_env_key(key) {
            return Err(ErgataiError::InvalidArgument(format!(
                "invalid env var key: {}",
                key
            )));
        }

        // Check for NUL bytes
        if key.contains('\0') || value.contains('\0') {
            return Err(ErgataiError::InvalidArgument(format!(
                "env var contains NUL byte: {}",
                key
            )));
        }

        // Check size limit
        if value.len() > MAX_ENV_VALUE_BYTES {
            return Err(ErgataiError::InvalidArgument(format!(
                "env var value exceeds maximum allowed length ({} bytes): {}",
                MAX_ENV_VALUE_BYTES, key
            )));
        }
    }

    // Validate structured fields
    for (field, value) in [("provider", &config.provider), ("model", &config.model)] {
        if let Some(v) = value {
            if v.contains('\0') {
                return Err(ErgataiError::InvalidArgument(format!(
                    "global config `{field}` must not contain NUL bytes"
                )));
            }
            if v.len() > MAX_ENV_VALUE_BYTES {
                return Err(ErgataiError::InvalidArgument(format!(
                    "global config `{field}` exceeds the maximum allowed length ({} bytes)",
                    MAX_ENV_VALUE_BYTES
                )));
            }
        }
    }

    Ok(())
}

/// Check if an env var key is well-formed (POSIX-style).
fn is_well_formed_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Save the global agent configuration to disk.
///
/// Validates the config, strips empty values, and writes with restricted permissions (0o600).
pub fn save_global_agent_config(
    config: &GlobalAgentConfig,
) -> Result<GlobalAgentConfigSaveResult, ErgataiError> {
    // Validate first
    validate_global_config(config).map_err(|e| {
        ErgataiError::ConfigError(ConfigError::ValidationFailed {
            reason: e.to_string(),
        })
    })?;

    // Strip empty values
    let mut cleaned = config.clone();
    cleaned.env_vars.retain(|_, v| !v.is_empty());

    // Normalize blank/whitespace-only values to None
    if let Some(ref provider) = cleaned.provider {
        if provider.trim().is_empty() {
            cleaned.provider = None;
        }
    }
    if let Some(ref model) = cleaned.model {
        if model.trim().is_empty() {
            cleaned.model = None;
        }
    }
    if let Some(ref runtime) = cleaned.preferred_runtime {
        if runtime.trim().is_empty() {
            cleaned.preferred_runtime = None;
        }
    }

    let path = global_config_path()?;
    let content = serde_json::to_string_pretty(&cleaned)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ParseFailed { source: e }))?;

    // Write with restricted permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);

        let mut file = options
            .open(&path)
            .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

        use std::io::Write;
        file.write_all(content.as_bytes())
            .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;
    }

    #[cfg(not(unix))]
    {
        fs::write(&path, &content)
            .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;
    }

    Ok(GlobalAgentConfigSaveResult { config: cleaned })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_global_config_rejects_derived_keys() {
        let mut config = GlobalAgentConfig::default();
        config
            .env_vars
            .insert("GOOSE_MODEL".to_string(), "test".to_string());

        let result = validate_global_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("GOOSE_MODEL"));
    }

    #[test]
    fn test_validate_global_config_rejects_nul_bytes() {
        let mut config = GlobalAgentConfig::default();
        config
            .env_vars
            .insert("TEST_KEY".to_string(), "test\0value".to_string());

        let result = validate_global_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("NUL"));
    }

    #[test]
    fn test_validate_global_config_rejects_invalid_keys() {
        let mut config = GlobalAgentConfig::default();
        config
            .env_vars
            .insert("INVALID KEY".to_string(), "value".to_string());

        let result = validate_global_config(&config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid env var key"));
    }

    #[test]
    fn test_is_well_formed_env_key() {
        assert!(is_well_formed_env_key("VALID_KEY"));
        assert!(is_well_formed_env_key("VALID_KEY_123"));
        assert!(is_well_formed_env_key("_VALID"));
        assert!(!is_well_formed_env_key("INVALID KEY"));
        assert!(!is_well_formed_env_key("123_INVALID"));
        assert!(!is_well_formed_env_key("INVALID=KEY"));
    }

    #[test]
    fn test_global_config_default() {
        let config = GlobalAgentConfig::default();
        assert!(config.env_vars.is_empty());
        assert!(config.provider.is_none());
        assert!(config.model.is_none());
        assert!(config.preferred_runtime.is_none());
    }
}
