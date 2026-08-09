use napi_derive::napi;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use url::Url;

use crate::error::{ConfigError, ErgataiError, ErgataiResult};

/// Normalize agent command to canonical identity.
///
/// Examples:
/// - "Claude_Code" → "claude-code"
/// - "/usr/local/bin/claude-agent-acp" → "claude-agent-acp"
/// - "C:\Program Files\Codex\codex.exe" → "codex"
pub fn normalize_agent_command_identity(command: &str) -> String {
    let normalized = command.trim().replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let basename = trimmed
        .rsplit('/')
        .next()
        .expect("rsplit always yields at least one element");
    let lower = basename.to_ascii_lowercase();
    // Windows resolves commands through `.exe` binaries and npm's `.cmd`/`.bat`
    // shims; all three name the same runtime identity.
    let stem = [".exe", ".cmd", ".bat"]
        .iter()
        .find_map(|extension| lower.strip_suffix(extension))
        .unwrap_or(&lower);
    stem.chars()
        .map(|character| match character {
            ' ' | '_' => '-',
            _ => character,
        })
        .collect()
}

/// Get default arguments for a known agent type.
///
/// Returns `Some(default_args)` for recognized agents, `None` for unknown agents.
///
/// # Known agents
/// - `goose` → `["acp"]`
/// - `codex`, `codex-acp`, `claude-code`, `claude-agent-acp` → `[]`
fn default_agent_args(command: &str) -> Option<Vec<String>> {
    match normalize_agent_command_identity(command).as_str() {
        "goose" => Some(vec!["acp".to_string()]),
        "codex" | "codex-acp" | "claude-agent-acp" | "claude-code-acp" | "claude-code"
        | "claudecode" => Some(Vec::new()),
        _ => None,
    }
}

/// Get default environment variables for a known agent type.
///
/// Returns a slice of (key, value) pairs to set for the agent process.
/// These defaults can be overridden by explicit persona env or inherited parent env.
///
/// # Known agents
/// - `hermes`, `hermes-agent`, `hermes-acp` → `[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")]`
///   (Hermes starts all profile-configured MCP servers on initialize, which can exhaust
///   the startup budget. This skips that unrelated global startup.)
pub fn default_agent_env(command: &str) -> &'static [(&'static str, &'static str)] {
    match normalize_agent_command_identity(command).as_str() {
        "hermes" | "hermes-agent" | "hermes-acp" => &[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")],
        _ => &[],
    }
}

/// Claude Code settings.json structure (partial — we only need the `env` field)
#[derive(Deserialize, Default)]
struct ClaudeSettings {
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Read environment variables from Claude Code's settings.json (~/.claude/settings.json).
///
/// Claude Code stores user-configured environment variables (like ANTHROPIC_AUTH_TOKEN,
/// ANTHROPIC_BASE_URL) in ~/.claude/settings.json. When spawning claude-agent-acp,
/// we need to pass these env vars so the subprocess can authenticate.
///
/// Returns an empty HashMap if the file doesn't exist or can't be parsed.
pub fn read_claude_settings_env() -> HashMap<String, String> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            tracing::warn!("[DEBUG] HOME env not set, cannot read Claude settings");
            return HashMap::new();
        }
    };

    let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");
    tracing::info!("[DEBUG] Checking Claude settings at: {:?} (HOME={})", settings_path, home);

    if !settings_path.exists() {
        tracing::warn!("[DEBUG] Claude settings file does not exist: {:?}", settings_path);
        return HashMap::new();
    }

    match std::fs::read_to_string(&settings_path) {
        Ok(contents) => {
            tracing::info!("[DEBUG] Read settings file, {} bytes", contents.len());
            match serde_json::from_str::<ClaudeSettings>(&contents) {
                Ok(settings) => {
                    tracing::info!(
                        "[DEBUG] Parsed Claude settings: {} env vars found: {:?}",
                        settings.env.len(),
                        settings.env.keys().collect::<Vec<_>>()
                    );
                    settings.env
                }
                Err(e) => {
                    tracing::warn!("[DEBUG] Failed to parse Claude settings JSON: {}", e);
                    HashMap::new()
                }
            }
        }
        Err(e) => {
            tracing::warn!("[DEBUG] Failed to read Claude settings file: {}", e);
            HashMap::new()
        }
    }
}

/// Build environment variables from runtime metadata and global config.
///
/// This function maps the global provider/model to agent-specific environment variables
/// based on the runtime metadata. For example:
/// - Goose: GOOSE_PROVIDER, GOOSE_MODEL
/// - Claude: provider_locked (no injection, Claude only supports Anthropic)
///
/// Precedence (low → high):
/// GLOBAL < per-agent config < runtime metadata injection
///
/// Returns a vector of (key, value) pairs to inject into the agent process environment.
pub fn runtime_metadata_env_vars(
    model_env_var: Option<&str>,
    provider_env_var: Option<&str>,
    provider_locked: bool,
    effective_model: Option<&str>,
    effective_provider: Option<&str>,
) -> Vec<(String, String)> {
    let mut vars = Vec::new();

    // Inject model env var if the runtime supports it and we have a value
    if let (Some(env_key), Some(model)) = (model_env_var, effective_model) {
        if !model.is_empty() {
            vars.push((env_key.to_string(), model.to_string()));
        }
    }

    // Inject provider env var if:
    // 1. The runtime supports it (not provider_locked)
    // 2. We have a value
    if !provider_locked {
        if let (Some(env_key), Some(provider)) = (provider_env_var, effective_provider) {
            if !provider.is_empty() {
                vars.push((env_key.to_string(), provider.to_string()));
            }
        }
    }

    vars
}

/// Build the `CODEX_CONFIG` environment variable that enables full outbound
/// network access in Codex's macOS Seatbelt sandbox.
///
/// Codex sandboxes MCP subprocesses behind a Seatbelt sandbox that blocks all
/// outbound network by default. Without this env var, requests are blocked.
///
/// Returns `Some(("CODEX_CONFIG", "{\"sandbox_workspace_write\":{\"network_access\":true}}"))` for
/// Codex agents, or `None` for non-Codex agents or when the relay URL cannot be parsed.
pub fn codex_network_env(agent_command: &str, relay_url: &str) -> Option<(String, String)> {
    match normalize_agent_command_identity(agent_command).as_str() {
        "codex" | "codex-acp" => {}
        _ => return None,
    }

    // Validate the relay URL before injecting broader network access.
    let host = match Url::parse(relay_url) {
        Ok(u) => match u.host_str() {
            Some(h) => h.to_owned(),
            None => {
                tracing::warn!(
                    relay_url,
                    "codex network config: no host in relay URL — skipping injection"
                );
                return None;
            }
        },
        Err(e) => {
            tracing::warn!(relay_url, error = %e, "codex network config: failed to parse relay URL — skipping injection");
            return None;
        }
    };

    tracing::debug!(host, "injecting CODEX_CONFIG network_access for relay host");

    Some((
        "CODEX_CONFIG".into(),
        "{\"sandbox_workspace_write\":{\"network_access\":true}}".into(),
    ))
}

/// Normalize agent arguments, applying defaults for known agents if empty.
pub fn normalize_agent_args(command: &str, agent_args: Vec<String>) -> Vec<String> {
    let normalized = agent_args
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();

    let Some(default_args) = default_agent_args(command) else {
        return normalized;
    };

    if normalized.is_empty() {
        return default_args;
    }

    // Older callers relied on the Goose-specific default even for runtimes like
    // Codex and Claude. Treat that legacy fallback as "no args" for zero-arg
    // providers so desktop- and env-based launches behave the same way.
    if normalized.len() == 1 && normalized[0].eq_ignore_ascii_case("acp") && default_args.is_empty()
    {
        return default_args;
    }

    normalized
}

/// Agent 配置（包含表单字段）
#[napi(object)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,

    // 表单字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// 自定义提示词 / persona
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Agent 类型标识（claude-code / codex / opencode / custom）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// 头像（base64 data URL 或文件路径）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// 获取 agent 配置
pub fn get_agent_config(name: &str) -> ErgataiResult<AgentConfig> {
    let config_path = get_config_path(name)?;
    if !config_path.exists() {
        return Err(ErgataiError::AgentNotFound(format!("Agent config not found: {}", name)));
    }
    let content = std::fs::read_to_string(&config_path)?;
    let mut config: AgentConfig = serde_json::from_str(&content)?;

    // Apply agent normalization
    normalize_agent_config(&mut config);

    Ok(config)
}

/// Apply agent normalization to an agent config.
///
/// This ensures:
/// - Command is normalized to canonical identity (e.g., "Claude_Code" → "claude-code")
/// - Args are normalized (empty args removed, defaults applied for known agents)
/// - Default environment variables are applied for known agents (e.g., hermes)
pub fn normalize_agent_config(config: &mut AgentConfig) {
    // Normalize command identity
    let normalized_command = normalize_agent_command_identity(&config.command);
    tracing::debug!(
        original = %config.command,
        normalized = %normalized_command,
        "Normalizing agent command"
    );

    // Normalize args (apply defaults for known agents if empty)
    let normalized_args = normalize_agent_args(&normalized_command, config.args.clone());
    if normalized_args != config.args {
        tracing::debug!(
            agent = %config.name,
            original = ?config.args,
            normalized = ?normalized_args,
            "Normalizing agent args"
        );
        config.args = normalized_args;
    }

    // Apply default environment variables for known agents
    for &(key, value) in default_agent_env(&normalized_command) {
        if !config.env.contains_key(key) {
            tracing::debug!(
                agent = %config.name,
                key = %key,
                value = %value,
                "Applying default agent env"
            );
            config.env.insert(key.to_string(), value.to_string());
        }
    }
}

/// 保存 agent 配置
pub fn save_agent_config(config: &AgentConfig) -> ErgataiResult<()> {
    let config_dir = get_config_dir()?;
    std::fs::create_dir_all(&config_dir)?;

    let config_path = get_config_path(&config.name)?;
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&config_path, content)?;

    // 设置文件权限为 600（仅所有者可读写）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&config_path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&config_path, perms)?;
    }

    Ok(())
}

fn get_config_dir() -> ErgataiResult<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or(ConfigError::DirectoryNotFound)?;
    Ok(config_dir.join("ergatai").join("agents"))
}

fn get_config_path(name: &str) -> ErgataiResult<PathBuf> {
    Ok(get_config_dir()?.join(format!("{}.json", name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_agent_command_identity ──

    #[test]
    fn test_normalize_command_identity_basic() {
        assert_eq!(normalize_agent_command_identity("claude"), "claude");
        assert_eq!(normalize_agent_command_identity("Claude"), "claude");
        assert_eq!(normalize_agent_command_identity("CLAUDE"), "claude");
    }

    #[test]
    fn test_normalize_command_identity_underscore_to_dash() {
        assert_eq!(normalize_agent_command_identity("claude_code"), "claude-code");
        assert_eq!(normalize_agent_command_identity("Claude_Code"), "claude-code");
    }

    #[test]
    fn test_normalize_command_identity_with_path() {
        assert_eq!(
            normalize_agent_command_identity("/usr/local/bin/claude"),
            "claude"
        );
        assert_eq!(
            normalize_agent_command_identity("/usr/local/bin/claude-code"),
            "claude-code"
        );
        assert_eq!(
            normalize_agent_command_identity("/usr/local/bin/claude_code"),
            "claude-code"
        );
    }

    #[test]
    fn test_normalize_command_identity_windows() {
        // Windows paths
        assert_eq!(
            normalize_agent_command_identity(r"C:\Users\test\claude.exe"),
            "claude"
        );
        assert_eq!(
            normalize_agent_command_identity(r"C:\Program Files\Claude\claude-agent-acp.cmd"),
            "claude-agent-acp"
        );
    }

    #[test]
    fn test_normalize_command_identity_with_extension() {
        assert_eq!(normalize_agent_command_identity("claude.sh"), "claude.sh");
        assert_eq!(normalize_agent_command_identity("claude.exe"), "claude");
        assert_eq!(normalize_agent_command_identity("claude.cmd"), "claude");
        assert_eq!(normalize_agent_command_identity("claude.bat"), "claude");
    }

    // ── normalize_agent_args ──

    #[test]
    fn test_normalize_agent_args_empty_for_known_agent() {
        // Known agent with empty args → apply defaults
        assert_eq!(normalize_agent_args("goose", vec![]), vec!["acp"]);
        assert_eq!(normalize_agent_args("codex", vec![]), vec![] as Vec<&str>);
        assert_eq!(normalize_agent_args("claude-code", vec![]), vec![] as Vec<&str>);
    }

    #[test]
    fn test_normalize_agent_args_unknown_agent() {
        // Unknown agent → pass through as-is
        assert_eq!(
            normalize_agent_args("custom-agent", vec!["--flag".to_string()]),
            vec!["--flag"]
        );
        assert_eq!(normalize_agent_args("custom-agent", vec![]), vec![] as Vec<&str>);
    }

    #[test]
    fn test_normalize_agent_args_legacy_acp_fallback() {
        // Legacy: single "acp" arg for zero-arg provider → empty
        assert_eq!(
            normalize_agent_args("codex", vec!["acp".to_string()]),
            vec![] as Vec<&str>
        );
        // But for goose (which expects "acp"), it should pass through
        assert_eq!(
            normalize_agent_args("goose", vec!["acp".to_string()]),
            vec!["acp"]
        );
    }

    #[test]
    fn test_normalize_agent_args_filters_empty() {
        // Empty strings should be filtered out
        assert_eq!(
            normalize_agent_args("codex", vec!["".to_string(), "--flag".to_string(), "".to_string()]),
            vec!["--flag"]
        );
    }

    #[test]
    fn test_normalize_agent_args_trims_whitespace() {
        assert_eq!(
            normalize_agent_args("codex", vec!["  --flag  ".to_string()]),
            vec!["--flag"]
        );
    }

    // ── default_agent_env ──

    #[test]
    fn test_default_agent_env_hermes() {
        let env = default_agent_env("hermes");
        assert_eq!(
            env,
            &[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")]
        );

        let env = default_agent_env("hermes-agent");
        assert_eq!(env, &[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")]);

        let env = default_agent_env("hermes-acp");
        assert_eq!(env, &[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")]);
    }

    #[test]
    fn test_default_agent_env_unknown() {
        let env = default_agent_env("claude-code");
        assert!(env.is_empty());

        let env = default_agent_env("unknown-agent");
        assert!(env.is_empty());
    }

    // ── codex_network_env ──

    #[test]
    fn test_codex_network_env_for_codex() {
        let result = codex_network_env("codex", "wss://relay.example.com");
        assert!(result.is_some());
        let (key, _value) = result.unwrap();
        assert_eq!(key, "CODEX_CONFIG");
    }

    #[test]
    fn test_codex_network_env_not_for_others() {
        assert!(codex_network_env("claude-code", "wss://relay.example.com").is_none());
        assert!(codex_network_env("goose", "wss://relay.example.com").is_none());
    }

    #[test]
    fn test_codex_network_env_invalid_url() {
        assert!(codex_network_env("codex", "not-a-url").is_none());
        assert!(codex_network_env("codex", "").is_none());
    }

    // ── runtime_metadata_env_vars ──

    #[test]
    fn test_runtime_metadata_env_vars_goose() {
        // Goose with provider and model
        let vars = runtime_metadata_env_vars(
            Some("GOOSE_MODEL"),
            Some("GOOSE_PROVIDER"),
            false,
            Some("claude-3-opus"),
            Some("anthropic"),
        );
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&("GOOSE_MODEL".to_string(), "claude-3-opus".to_string())));
        assert!(vars.contains(&("GOOSE_PROVIDER".to_string(), "anthropic".to_string())));
    }

    #[test]
    fn test_runtime_metadata_env_vars_claude_locked() {
        // Claude is provider_locked, so provider should NOT be injected
        let vars = runtime_metadata_env_vars(
            None,
            None,
            true, // provider_locked
            Some("claude-3-opus"),
            Some("anthropic"),
        );
        assert!(vars.is_empty()); // No env vars for Claude
    }

    #[test]
    fn test_runtime_metadata_env_vars_empty_values() {
        // Empty values should not be injected
        let vars = runtime_metadata_env_vars(
            Some("GOOSE_MODEL"),
            Some("GOOSE_PROVIDER"),
            false,
            Some(""),
            Some(""),
        );
        assert!(vars.is_empty());
    }

    #[test]
    fn test_runtime_metadata_env_vars_none_values() {
        // None values should not be injected
        let vars = runtime_metadata_env_vars(
            Some("GOOSE_MODEL"),
            Some("GOOSE_PROVIDER"),
            false,
            None,
            None,
        );
        assert!(vars.is_empty());
    }

    #[test]
    fn test_runtime_metadata_env_vars_partial() {
        // Only model provided
        let vars = runtime_metadata_env_vars(
            Some("GOOSE_MODEL"),
            Some("GOOSE_PROVIDER"),
            false,
            Some("claude-3-opus"),
            None,
        );
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0], ("GOOSE_MODEL".to_string(), "claude-3-opus".to_string()));
    }
}
