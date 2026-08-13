//! Custom harness definitions for user-defined ACP agents.
//!
//! Users can define custom agents by creating JSON files in the custom harnesses directory.
//! These are loaded alongside the builtin runtimes during discovery.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ErgataiError};

/// Custom harness definition loaded from JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessDefinition {
    /// Unique identifier for this harness (must match filename).
    pub id: String,

    /// Human-readable label.
    pub label: String,

    /// Command to launch the agent (e.g., "my-agent", "/path/to/agent").
    pub command: String,

    /// Default arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set when launching the agent.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// URL with installation instructions (optional).
    #[serde(default)]
    pub install_instructions_url: String,

    /// Hint text for installation (optional).
    #[serde(default)]
    pub install_hint: String,
}

/// Get the path to the custom harnesses directory.
fn custom_harnesses_dir() -> Result<PathBuf, ErgataiError> {
    let app_data = dirs::config_dir()
        .ok_or(ErgataiError::ConfigError(ConfigError::DirectoryNotFound))?
        .join("ergatai")
        .join("custom_harnesses");

    fs::create_dir_all(&app_data).map_err(|e| {
        ErgataiError::ConfigError(ConfigError::ValidationFailed {
            reason: format!(
                "Failed to create custom harnesses directory {}: {}",
                app_data.display(),
                e
            ),
        })
    })?;

    Ok(app_data)
}

/// Load all custom harness definitions from the custom harnesses directory.
///
/// Returns an empty vector if the directory doesn't exist or contains no valid JSON files.
pub fn load_custom_harnesses() -> Result<Vec<HarnessDefinition>, ErgataiError> {
    let dir = custom_harnesses_dir()?;

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut harnesses = Vec::new();

    for entry in fs::read_dir(&dir)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?
    {
        let entry =
            entry.map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

        let path = entry.path();

        // Only process JSON files
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        match load_harness_from_file(&path) {
            Ok(harness) => harnesses.push(harness),
            Err(e) => {
                // Log error but continue loading other harnesses
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to load custom harness, skipping"
                );
            }
        }
    }

    Ok(harnesses)
}

/// Load a single harness definition from a JSON file.
fn load_harness_from_file(path: &Path) -> Result<HarnessDefinition, ErgataiError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

    let harness: HarnessDefinition = serde_json::from_str(&content)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ParseFailed { source: e }))?;

    // Validate that the id matches the filename
    let expected_id = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
        ErgataiError::ConfigError(ConfigError::ValidationFailed {
            reason: "Invalid harness filename".to_string(),
        })
    })?;

    if harness.id != expected_id {
        return Err(ErgataiError::ConfigError(ConfigError::ValidationFailed {
            reason: format!(
                "Harness id '{}' does not match filename '{}'",
                harness.id, expected_id
            ),
        }));
    }

    // Validate id format
    if !is_valid_harness_id(&harness.id) {
        return Err(ErgataiError::ConfigError(ConfigError::ValidationFailed {
            reason: format!(
                "Invalid harness id '{}': must match [a-z0-9_][a-z0-9_-]*",
                harness.id
            ),
        }));
    }

    Ok(harness)
}

/// Save a harness definition to a JSON file.
///
/// The filename is derived from the harness id.
pub fn save_custom_harness(harness: &HarnessDefinition) -> Result<(), ErgataiError> {
    // Validate id format
    if !is_valid_harness_id(&harness.id) {
        return Err(ErgataiError::InvalidArgument(format!(
            "Invalid harness id '{}': must match [a-z0-9_][a-z0-9_-]*",
            harness.id
        )));
    }

    let dir = custom_harnesses_dir()?;
    let path = dir.join(format!("{}.json", harness.id));

    let content = serde_json::to_string_pretty(harness)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ParseFailed { source: e }))?;

    fs::write(&path, content)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

    Ok(())
}

/// Delete a custom harness by id.
pub fn delete_custom_harness(id: &str) -> Result<(), ErgataiError> {
    let dir = custom_harnesses_dir()?;
    let path = dir.join(format!("{}.json", id));

    if !path.exists() {
        return Err(ErgataiError::AgentNotFound(format!(
            "Custom harness '{}' not found",
            id
        )));
    }

    fs::remove_file(&path)
        .map_err(|e| ErgataiError::ConfigError(ConfigError::ReadFailed { source: e }))?;

    Ok(())
}

/// Check if a harness id is valid.
///
/// Valid ids match the pattern: [a-z0-9_][a-z0-9_-]*
fn is_valid_harness_id(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }

    let mut chars = id.chars();

    // First character must be lowercase letter, digit, or underscore
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' => {}
        _ => return false,
    }

    // Remaining characters can also include hyphens
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_harness_id() {
        assert!(is_valid_harness_id("my-agent"));
        assert!(is_valid_harness_id("my_agent"));
        assert!(is_valid_harness_id("agent123"));
        assert!(is_valid_harness_id("_agent"));
        assert!(is_valid_harness_id("123agent"));
        assert!(!is_valid_harness_id(""));
        assert!(!is_valid_harness_id("My-Agent")); // uppercase
        assert!(!is_valid_harness_id("-agent")); // starts with hyphen
        assert!(!is_valid_harness_id("agent.name")); // contains dot
    }

    #[test]
    fn test_harness_definition_serialization() {
        let harness = HarnessDefinition {
            id: "test-agent".to_string(),
            label: "Test Agent".to_string(),
            command: "test-agent".to_string(),
            args: vec!["--acp".to_string()],
            env: BTreeMap::new(),
            install_instructions_url: "https://example.com/install".to_string(),
            install_hint: "Install from example.com".to_string(),
        };

        let json = serde_json::to_string(&harness).unwrap();
        let deserialized: HarnessDefinition = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, harness.id);
        assert_eq!(deserialized.label, harness.label);
        assert_eq!(deserialized.command, harness.command);
        assert_eq!(deserialized.args, harness.args);
    }
}
