//! Hosted Agent Configuration
//!
//! Manages user-created agent configurations hosted by Ergatai.
//! Each hosted agent has its own directory under `~/.config/ergatai/agents/{name}/`
//! containing a `settings.json` file.
//!
//! The settings.json contains:
//! - An `ergatai` group with system fields (agentBase, displayName, proxy, avatar)
//! - The rest is the original agent's native config format, passed through as-is
//!
//! On startup, Ergatai extracts the `ergatai` group and strips it from the config,
//! passing only the original agent config to the agent subprocess.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{ConfigError, ErgataiError, ErgataiResult};

/// Ergatai system metadata extracted from the `ergatai` group in settings.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErgataiAgentMeta {
    /// Underlying agent identifier: "claude", "codex", "goose", "hermes", etc.
    pub agent_base: String,

    /// Display name shown in UI. Falls back to agent directory name if not set.
    #[serde(default)]
    pub display_name: Option<String>,

    /// Network proxy address, e.g. "http://127.0.0.1:7890"
    #[serde(default)]
    pub proxy: Option<String>,

    /// Avatar path (relative to agent dir or absolute). Auto-assigned if not set.
    #[serde(default)]
    pub avatar: Option<String>,
}

/// A hosted agent configuration: system metadata + original agent config.
///
/// The `agent_config` field contains everything from settings.json
/// EXCEPT the `ergatai` group — this is the native agent config
/// that gets passed to the agent subprocess.
#[derive(Debug, Clone)]
pub struct HostedAgentConfig {
    /// The agent directory name (used as unique identifier)
    pub name: String,

    /// Full path to the agent directory
    pub dir_path: PathBuf,

    /// Ergatai system metadata
    pub meta: ErgataiAgentMeta,

    /// Original agent config (everything except `ergatai` key)
    /// Serialized as JSON Value to preserve the agent's native format
    pub agent_config: serde_json::Value,
}

/// Agent base → default command mapping
pub fn default_command_for_base(agent_base: &str) -> Option<&'static str> {
    match agent_base {
        "claude" => Some("claude-agent-acp"),
        "codex" => Some("codex-acp"),
        "goose" => Some("goose"),
        "hermes" => Some("hermes-acp"),
        "opencode" => Some("opencode"),
        "kimi" => Some("kimi"),
        _ => None,
    }
}

/// Agent base → (config_dir_env_var, sub_directory)
///
/// Returns the environment variable name used by each agent to specify
/// its configuration directory, and the sub-directory name within that
/// directory (if any).
///
/// This enables config isolation: each hosted agent gets its own isolated
/// config directory, preventing it from reading global user configs.
///
/// # Examples
///
/// - Claude: `CLAUDE_CONFIG_DIR` → `{agent_dir}/.config/`
/// - OpenCode: `OPENCODE_CONFIG_DIR` → `{agent_dir}/.config/`
/// - Goose: `XDG_CONFIG_HOME` → `{agent_dir}/.config/goose/`
/// - Codex: `XDG_CONFIG_HOME` → `{agent_dir}/.config/codex/`
pub fn config_dir_env_for_agent(agent_base: &str) -> Option<(&'static str, &'static str)> {
    match agent_base {
        "claude" => Some(("CLAUDE_CONFIG_DIR", "")),
        "opencode" => Some(("OPENCODE_CONFIG_DIR", "")),
        "goose" => Some(("XDG_CONFIG_HOME", "goose")),
        "codex" => Some(("XDG_CONFIG_HOME", "codex")),
        "hermes" => Some(("XDG_CONFIG_HOME", "hermes")),
        "kimi" => Some(("XDG_CONFIG_HOME", "kimi")),
        _ => None,
    }
}

/// Validate an agent name (no path traversal, not empty).
pub(crate) fn validate_agent_name(name: &str) -> Result<(), ErgataiError> {
    if name.is_empty() {
        return Err(ErgataiError::InvalidArgument(
            "Agent name cannot be empty".to_string(),
        ));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(ErgataiError::InvalidArgument(format!(
            "Invalid agent name: '{name}' (must not contain path separators or '..')"
        )));
    }
    Ok(())
}

/// Get the base directory for hosted agent configs.
///
/// Returns `~/.config/ergatai/agents/`
pub fn hosted_agents_dir() -> ErgataiResult<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| {
            ErgataiError::InvalidArgument("HOME or USERPROFILE env var not set".to_string())
        })?;

    Ok(PathBuf::from(home)
        .join(".config")
        .join("ergatai")
        .join("agents"))
}

/// List all hosted agent names.
///
/// Scans the agents directory and returns directory names that contain
/// a valid `settings.json` file.
pub fn list_hosted_agents() -> ErgataiResult<Vec<String>> {
    let base_dir = hosted_agents_dir()?;

    if !base_dir.exists() {
        return Ok(vec![]);
    }

    let mut names = Vec::new();

    let entries =
        std::fs::read_dir(&base_dir).map_err(|e| ConfigError::ReadFailed { source: e })?;

    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::ReadFailed { source: e })?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let settings_path = path.join("settings.json");
        if settings_path.exists() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                names.push(name.to_string());
            }
        }
    }

    names.sort();
    Ok(names)
}

/// Load a hosted agent configuration by name.
///
/// Reads `~/.config/ergatai/agents/{name}/settings.json`, extracts the `ergatai`
/// group as system metadata, and returns the remaining JSON as the agent config.
pub fn load_hosted_agent(name: &str) -> ErgataiResult<HostedAgentConfig> {
    validate_agent_name(name)?;

    let base_dir = hosted_agents_dir()?;
    let agent_dir = base_dir.join(name);
    let settings_path = agent_dir.join("settings.json");

    if !settings_path.exists() {
        return Err(ErgataiError::AgentNotFound(name.to_string()));
    }

    // Read and parse
    let content = std::fs::read_to_string(&settings_path)
        .map_err(|e| ConfigError::ReadFailed { source: e })?;

    let mut full_config: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| ConfigError::ParseFailed { source: e })?;

    // Extract ergatai group
    let ergatai_value = full_config
        .as_object_mut()
        .ok_or_else(|| ConfigError::ValidationFailed {
            reason: "settings.json root must be a JSON object".to_string(),
        })?
        .remove("ergatai")
        .ok_or_else(|| ConfigError::ValidationFailed {
            reason: format!(
                "settings.json for agent '{}' missing required 'ergatai' group",
                name
            ),
        })?;

    let meta: ErgataiAgentMeta = serde_json::from_value(ergatai_value)
        .map_err(|e| ConfigError::ParseFailed { source: e })?;

    // Validate agent_base
    if meta.agent_base.is_empty() {
        return Err(ConfigError::ValidationFailed {
            reason: format!("agent '{}' has empty agentBase in ergatai group", name),
        }
        .into());
    }

    info!(
        agent = name,
        agent_base = %meta.agent_base,
        display_name = ?meta.display_name,
        "Loaded hosted agent config"
    );

    // Optional: validate agent_config basic structure (catch common errors early)
    if let Some(env_val) = full_config.get("env") {
        if !env_val.is_object() {
            tracing::warn!(
                agent = name,
                "'env' field is not an object, will be ignored by agent subprocess"
            );
        }
    }

    // What remains is the original agent config
    Ok(HostedAgentConfig {
        name: name.to_string(),
        dir_path: agent_dir,
        meta,
        agent_config: full_config,
    })
}

/// Create a new hosted agent with the given name and settings.
///
/// Creates the directory and writes settings.json.
/// Returns error if a legacy agent with the same name already exists (prevents naming conflicts).
pub fn create_hosted_agent(name: &str, settings: &serde_json::Value) -> ErgataiResult<PathBuf> {
    validate_agent_name(name)?;

    // Check for legacy agent with same name (prevent naming conflicts)
    let legacy_path = crate::agent::config::get_config_path(name)?;
    if legacy_path.exists() {
        return Err(ErgataiError::InvalidArgument(format!(
            "Cannot create hosted agent '{}': a legacy agent with the same name already exists at {}",
            name,
            legacy_path.display()
        )));
    }

    // Validate ergatai group exists
    let ergatai = settings
        .get("ergatai")
        .ok_or_else(|| ConfigError::ValidationFailed {
            reason: "Settings must contain 'ergatai' group".to_string(),
        })?;

    let meta: ErgataiAgentMeta = serde_json::from_value(ergatai.clone())
        .map_err(|e| ConfigError::ParseFailed { source: e })?;

    if meta.agent_base.is_empty() {
        return Err(ConfigError::ValidationFailed {
            reason: "agentBase is required in ergatai group".to_string(),
        }
        .into());
    }

    let base_dir = hosted_agents_dir()?;
    let agent_dir = base_dir.join(name);

    if agent_dir.exists() {
        return Err(ErgataiError::InvalidArgument(format!(
            "Agent '{}' already exists at {}",
            name,
            agent_dir.display()
        )));
    }

    // Create directory
    std::fs::create_dir_all(&agent_dir).map_err(|e| ConfigError::ReadFailed { source: e })?;

    // Write settings.json (pretty-printed)
    let settings_path = agent_dir.join("settings.json");
    let content = serde_json::to_string_pretty(settings)
        .map_err(|e| ConfigError::ParseFailed { source: e })?;

    std::fs::write(&settings_path, content).map_err(|e| ConfigError::ReadFailed { source: e })?;

    info!(
        agent = name,
        agent_base = %meta.agent_base,
        path = %agent_dir.display(),
        "Created hosted agent"
    );

    Ok(agent_dir)
}

/// Delete a hosted agent by name.
///
/// Removes the entire agent directory.
pub fn delete_hosted_agent(name: &str) -> ErgataiResult<()> {
    validate_agent_name(name)?;

    let base_dir = hosted_agents_dir()?;
    let agent_dir = base_dir.join(name);

    if !agent_dir.exists() {
        return Err(ErgataiError::AgentNotFound(name.to_string()));
    }

    std::fs::remove_dir_all(&agent_dir).map_err(|e| ConfigError::ReadFailed { source: e })?;

    info!(agent = name, "Deleted hosted agent");
    Ok(())
}

/// Update an existing hosted agent's settings.
///
/// Validates the `ergatai` group schema completely (consistent with `create_hosted_agent`).
pub fn update_hosted_agent(name: &str, settings: &serde_json::Value) -> ErgataiResult<()> {
    validate_agent_name(name)?;

    let base_dir = hosted_agents_dir()?;
    let agent_dir = base_dir.join(name);
    let settings_path = agent_dir.join("settings.json");

    if !settings_path.exists() {
        return Err(ErgataiError::AgentNotFound(name.to_string()));
    }

    // Validate ergatai group exists in new settings
    let ergatai = settings
        .get("ergatai")
        .ok_or_else(|| ConfigError::ValidationFailed {
            reason: "Settings must contain 'ergatai' group".to_string(),
        })?;

    // Complete schema validation (consistent with create_hosted_agent)
    let meta: ErgataiAgentMeta = serde_json::from_value(ergatai.clone())
        .map_err(|e| ConfigError::ParseFailed { source: e })?;

    if meta.agent_base.is_empty() {
        return Err(ConfigError::ValidationFailed {
            reason: "agentBase cannot be empty in ergatai group".to_string(),
        }
        .into());
    }

    // Security: validate agent_base only contains safe characters
    if !meta
        .agent_base
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::ValidationFailed {
            reason: format!(
                "agent_base '{}' contains invalid characters (only alphanumeric, '-', '_' allowed)",
                meta.agent_base
            ),
        }
        .into());
    }

    let content = serde_json::to_string_pretty(settings)
        .map_err(|e| ConfigError::ParseFailed { source: e })?;

    std::fs::write(&settings_path, content).map_err(|e| ConfigError::ReadFailed { source: e })?;

    info!(agent = name, "Updated hosted agent config");
    Ok(())
}

/// Get the display name for a hosted agent.
///
/// Returns `meta.display_name` if set, otherwise the agent directory name.
pub fn display_name(config: &HostedAgentConfig) -> String {
    config
        .meta
        .display_name
        .clone()
        .unwrap_or_else(|| config.name.clone())
}

/// Get the resolved avatar path for a hosted agent.
///
/// If `meta.avatar` is a relative path, resolves it relative to the agent directory.
/// Returns None if no avatar is set.
///
/// # Security
///
/// If the avatar file exists, validates that the resolved path is within the agent
/// directory to prevent path traversal attacks. If the file doesn't exist yet,
/// returns the resolved path without validation (file may be uploaded later).
pub fn avatar_path(config: &HostedAgentConfig) -> Option<PathBuf> {
    let avatar = config.meta.avatar.as_ref()?;
    let path = Path::new(avatar);

    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        config.dir_path.join(path)
    };

    // Security: if file exists, validate it's within agent_dir
    // If file doesn't exist, skip validation (may be uploaded later)
    if resolved.exists() {
        if let (Ok(canonical), Ok(base_canonical)) =
            (resolved.canonicalize(), config.dir_path.canonicalize())
        {
            if !canonical.starts_with(&base_canonical) {
                tracing::warn!(
                    avatar = %resolved.display(),
                    agent_dir = %config.dir_path.display(),
                    "Avatar path escapes agent directory, rejecting"
                );
                return None;
            }
        }
    }

    Some(resolved)
}

/// Build proxy environment variables from the config.
///
/// Returns a map of HTTP_PROXY, HTTPS_PROXY env vars if a proxy is configured.
pub fn proxy_env(config: &HostedAgentConfig) -> HashMap<String, String> {
    let mut env = HashMap::new();

    if let Some(ref proxy) = config.meta.proxy {
        env.insert("HTTP_PROXY".to_string(), proxy.clone());
        env.insert("HTTPS_PROXY".to_string(), proxy.clone());
        env.insert("http_proxy".to_string(), proxy.clone());
        env.insert("https_proxy".to_string(), proxy.clone());
    }

    env
}

/// Convert a `HostedAgentConfig` to an `AgentConfig` suitable for spawning.
///
/// - `command` is derived from `agentBase` (e.g. "claude" → "claude-agent-acp")
/// - `env` merges proxy env + any `env` object from the trimmed agent config
/// - `display_name` / `avatar` / `proxy` come from the ergatai meta
///
/// The remaining agent config fields (model, theme, etc.) are agent-specific
/// and will be picked up by the agent subprocess if it reads settings.json.
///
/// # Errors
///
/// Returns an error if `agent_base` contains invalid characters (only alphanumeric,
/// '-', '_' are allowed).
pub fn to_agent_config(
    config: &HostedAgentConfig,
) -> ErgataiResult<crate::agent::config::AgentConfig> {
    use crate::agent::config::AgentConfig;

    // Security: validate agent_base only contains safe characters
    // This prevents arbitrary command execution if config is loaded from untrusted source
    if !config
        .meta
        .agent_base
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::ValidationFailed {
            reason: format!(
                "agent_base '{}' contains invalid characters (only alphanumeric, '-', '_' allowed)",
                config.meta.agent_base
            ),
        }
        .into());
    }

    // Determine command from agent_base
    let command = default_command_for_base(&config.meta.agent_base)
        .unwrap_or(&config.meta.agent_base)
        .to_string();

    // Build env: start with agent config's "env" object (if any), then overlay proxy
    let mut env = HashMap::new();

    // Extract env vars from the trimmed agent config
    if let Some(env_obj) = config.agent_config.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env_obj {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }

    // Overlay proxy env
    for (k, v) in proxy_env(config) {
        env.insert(k, v);
    }

    // Set isolated config directory for all agents to prevent reading global configs.
    // Each agent uses different env vars:
    // - Claude: CLAUDE_CONFIG_DIR
    // - OpenCode: OPENCODE_CONFIG_DIR
    // - Goose/Codex: XDG_CONFIG_HOME (Linux standard)
    if !env.contains_key("CLAUDE_CONFIG_DIR")
        && !env.contains_key("OPENCODE_CONFIG_DIR")
        && !env.contains_key("XDG_CONFIG_HOME")
    {
        if let Some((env_var, sub_dir)) = config_dir_env_for_agent(&config.meta.agent_base) {
            let config_dir = if sub_dir.is_empty() {
                config.dir_path.join(".config")
            } else {
                config.dir_path.join(".config").join(sub_dir)
            };
            // Ensure the directory exists — log warning if creation fails
            if let Err(e) = std::fs::create_dir_all(&config_dir) {
                tracing::warn!(
                    agent = %config.name,
                    agent_base = %config.meta.agent_base,
                    config_dir = %config_dir.display(),
                    error = %e,
                    "Failed to create isolated config directory — agent may read global configs"
                );
            }
            env.insert(
                env_var.to_string(),
                config_dir.to_string_lossy().to_string(),
            );
            tracing::debug!(
                agent = %config.name,
                agent_base = %config.meta.agent_base,
                env_var = env_var,
                config_dir = ?config_dir,
                "Set isolated config directory for agent"
            );
        }
    }

    // Extract common fields from the native agent config
    // These are passed to `build_acp_agent_config` which converts them to
    // agent-specific environment variables (e.g. ANTHROPIC_AUTH_TOKEN, OPENAI_API_KEY, etc.)
    let model = config
        .agent_config
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let provider = config
        .agent_config
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let api_key = config
        .agent_config
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let base_url = config
        .agent_config
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let persona = config
        .agent_config
        .get("persona")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let agent_config = AgentConfig {
        name: config.name.clone(),
        command,
        args: Vec::new(),
        env,
        display_name: config.meta.display_name.clone(),
        base_url,
        model,
        provider,
        api_key,
        proxy: config.meta.proxy.clone(),
        persona,
        agent_type: Some(config.meta.agent_base.clone()),
        avatar: config.meta.avatar.clone(),
    };

    Ok(agent_config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_default_command_for_base() {
        assert_eq!(default_command_for_base("claude"), Some("claude-agent-acp"));
        assert_eq!(default_command_for_base("codex"), Some("codex-acp"));
        assert_eq!(default_command_for_base("goose"), Some("goose"));
        assert_eq!(default_command_for_base("hermes"), Some("hermes-acp"));
        assert_eq!(default_command_for_base("unknown"), None);
    }

    #[test]
    fn test_ergatai_meta_deserialize() {
        let json = json!({
            "agentBase": "claude",
            "displayName": "My Claude",
            "proxy": "http://127.0.0.1:7890"
        });

        let meta: ErgataiAgentMeta = serde_json::from_value(json).unwrap();
        assert_eq!(meta.agent_base, "claude");
        assert_eq!(meta.display_name.as_deref(), Some("My Claude"));
        assert_eq!(meta.proxy.as_deref(), Some("http://127.0.0.1:7890"));
        assert!(meta.avatar.is_none());
    }

    #[test]
    fn test_config_trim() {
        let full = json!({
            "ergatai": {
                "agentBase": "claude",
                "displayName": "Test"
            },
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-test"
            },
            "model": "deepseek-v4-flash",
            "theme": "auto"
        });

        let mut config = full.clone();
        let ergatai_value = config.as_object_mut().unwrap().remove("ergatai").unwrap();
        let meta: ErgataiAgentMeta = serde_json::from_value(ergatai_value).unwrap();

        assert_eq!(meta.agent_base, "claude");

        // Remaining config should not have ergatai key
        assert!(config.get("ergatai").is_none());
        assert_eq!(config.get("model").unwrap(), "deepseek-v4-flash");
        assert_eq!(config.get("theme").unwrap(), "auto");
        assert!(config.get("env").unwrap().is_object());
    }

    #[test]
    fn test_name_validation() {
        assert!(validate_agent_name("my-agent").is_ok());
        assert!(validate_agent_name("claude_opus").is_ok());
        assert!(validate_agent_name("").is_err());
        assert!(validate_agent_name("../etc/passwd").is_err());
        assert!(validate_agent_name("foo/bar").is_err());
        assert!(validate_agent_name("foo\\bar").is_err());
    }

    #[test]
    fn test_proxy_env() {
        let config = HostedAgentConfig {
            name: "test".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: Some("http://127.0.0.1:7890".to_string()),
                avatar: None,
            },
            agent_config: json!({}),
        };

        let env = proxy_env(&config);
        assert_eq!(env.get("HTTP_PROXY").unwrap(), "http://127.0.0.1:7890");
        assert_eq!(env.get("HTTPS_PROXY").unwrap(), "http://127.0.0.1:7890");
        assert_eq!(env.get("http_proxy").unwrap(), "http://127.0.0.1:7890");
        assert_eq!(env.get("https_proxy").unwrap(), "http://127.0.0.1:7890");
    }

    #[test]
    fn test_proxy_env_empty() {
        let config = HostedAgentConfig {
            name: "test".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: None,
                avatar: None,
            },
            agent_config: json!({}),
        };

        let env = proxy_env(&config);
        assert!(env.is_empty());
    }

    #[test]
    fn test_display_name_fallback() {
        let config_no_display = HostedAgentConfig {
            name: "my-agent".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: None,
                avatar: None,
            },
            agent_config: json!({}),
        };
        assert_eq!(display_name(&config_no_display), "my-agent");

        let config_with_display = HostedAgentConfig {
            name: "my-agent".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: Some("My Custom Agent".to_string()),
                proxy: None,
                avatar: None,
            },
            agent_config: json!({}),
        };
        assert_eq!(display_name(&config_with_display), "My Custom Agent");
    }

    #[test]
    fn test_avatar_path() {
        let config_relative = HostedAgentConfig {
            name: "test".to_string(),
            dir_path: PathBuf::from("/home/user/.config/ergatai/agents/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: None,
                avatar: Some("./avatar.png".to_string()),
            },
            agent_config: json!({}),
        };
        assert_eq!(
            avatar_path(&config_relative),
            Some(PathBuf::from(
                "/home/user/.config/ergatai/agents/test/./avatar.png"
            ))
        );

        let config_absolute = HostedAgentConfig {
            name: "test".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: None,
                avatar: Some("/absolute/path/avatar.png".to_string()),
            },
            agent_config: json!({}),
        };
        assert_eq!(
            avatar_path(&config_absolute),
            Some(PathBuf::from("/absolute/path/avatar.png"))
        );

        let config_none = HostedAgentConfig {
            name: "test".to_string(),
            dir_path: PathBuf::from("/tmp/test"),
            meta: ErgataiAgentMeta {
                agent_base: "claude".to_string(),
                display_name: None,
                proxy: None,
                avatar: None,
            },
            agent_config: json!({}),
        };
        assert_eq!(avatar_path(&config_none), None);
    }
}
