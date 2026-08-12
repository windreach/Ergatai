use napi_derive::napi;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use url::Url;

use crate::error::{ConfigError, ErgataiError, ErgataiResult};
use crate::agent::runtime_metadata::{
    default_agent_args as runtime_default_agent_args, known_acp_runtime,
    normalize_command_identity as runtime_normalize_command_identity,
};
use crate::agent::global_config::load_global_agent_config;

/// Normalize agent command to canonical identity.
///
/// Delegates to `runtime_metadata::normalize_command_identity` (single source of truth).
pub fn normalize_agent_command_identity(command: &str) -> String {
    runtime_normalize_command_identity(command)
}

/// Get default arguments for a known agent type.
///
/// Delegates to `runtime_metadata::default_agent_args` (single source of truth).
fn default_agent_args(command: &str) -> Option<Vec<String>> {
    runtime_default_agent_args(command)
}

/// Get default environment variables for a known agent type.
///
/// Delegates to `runtime_metadata::known_acp_runtime().default_env` — single source of truth.
/// Returns a static slice of (key, value) pairs for known agents.
///
/// **Note:** `command` is normalized internally. Passing a pre-normalized command
/// (via `normalize_agent_command_identity`) avoids one redundant normalization.
///
/// Returns an empty slice for unknown agents.
pub fn default_agent_env(command: &str) -> &'static [(&'static str, &'static str)] {
    let normalized = normalize_agent_command_identity(command);
    known_acp_runtime(&normalized)
        .map(|rt| rt.default_env)
        .unwrap_or(&[])
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
            tracing::warn!("HOME env not set, cannot read Claude settings");
            return HashMap::new();
        }
    };

    let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");
    tracing::debug!(path = ?settings_path, home = %home, "Checking Claude settings");

    if !settings_path.exists() {
        tracing::warn!(path = ?settings_path, "Claude settings file does not exist");
        return HashMap::new();
    }

    match std::fs::read_to_string(&settings_path) {
        Ok(contents) => {
            tracing::debug!(bytes = contents.len(), "Read settings file");
            match serde_json::from_str::<ClaudeSettings>(&contents) {
                Ok(settings) => {
                    tracing::debug!(
                        env_count = settings.env.len(),
                        env_keys = ?settings.env.keys().collect::<Vec<_>>(),
                        "Parsed Claude settings"
                    );
                    settings.env
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to parse Claude settings JSON");
                    HashMap::new()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read Claude settings file");
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
/// **Note:** `agent_command` should be pre-normalized via `normalize_agent_command_identity`.
///
/// Returns `Some(("CODEX_CONFIG", "..."))` for Codex agents,
/// or `None` for non-Codex agents or when the relay URL cannot be parsed.
pub fn codex_network_env(agent_command: &str, relay_url: &str) -> Option<(String, String)> {
    match agent_command {
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

/// Map AgentConfig form fields to environment variables for the agent subprocess.
///
/// Handles api_key, base_url, proxy, and persona → env var mapping using
/// `KnownAcpRuntime` metadata for agent-specific env var names.
///
/// NOTE: model and provider are NOT handled here — they are injected by
/// `build_acp_agent_config` via `runtime_metadata_env_vars` with proper
/// global/per-agent precedence.
///
/// Returns a vector of (key, value) pairs to inject.
pub fn agent_config_to_env(config: &AgentConfig) -> Vec<(String, String)> {
    let command = normalize_agent_command_identity(&config.command);
    let mut vars = Vec::new();

    // Look up the known runtime metadata for this agent
    let runtime = known_acp_runtime(&command);

    // API Key → agent-specific env var (from metadata, with fallback)
    if let Some(ref api_key) = config.api_key {
        if !api_key.is_empty() {
            let env_key = runtime
                .and_then(|r| r.api_key_env_var)
                .unwrap_or("API_KEY");
            vars.push((env_key.to_string(), api_key.clone()));
        }
    }

    // Base URL → agent-specific env var (from metadata, with fallback)
    if let Some(ref base_url) = config.base_url {
        if !base_url.is_empty() {
            let env_key = runtime
                .and_then(|r| r.base_url_env_var)
                .unwrap_or("BASE_URL");
            vars.push((env_key.to_string(), base_url.clone()));
        }
    }

    // Proxy → standard HTTP proxy env vars (universal)
    if let Some(ref proxy) = config.proxy {
        if !proxy.is_empty() {
            vars.push(("HTTP_PROXY".to_string(), proxy.clone()));
            vars.push(("HTTPS_PROXY".to_string(), proxy.clone()));
        }
    }

    // Persona → generic env var (agents can opt-in to read it)
    if let Some(ref persona) = config.persona {
        if !persona.is_empty() {
            vars.push(("ERGATAI_PERSONA".to_string(), persona.clone()));
        }
    }

    vars
}

/// Build a fully-configured `AcpAgentConfig` from an `AgentConfig`.
///
/// This is the single entry point for constructing an ACP agent config with all
/// environment variable injection applied. It encapsulates the full injection chain:
///
/// ```text
/// ① global_config.env_vars    ← lowest priority (global fallback)
/// ② config.env                ← per-agent env map (from JSON)
/// ③ agent_config_to_env()     ← form fields → env vars (api_key, base_url, proxy, persona)
/// ④ runtime_metadata_env_vars() ← global model/provider → agent-specific env vars
/// ⑤ default_agent_env()       ← metadata default_env (GOOSE_MODE, HERMES_*)
/// ⑥ claude_settings_env()     ← ~/.claude/settings.json (Claude only)
/// ⑦ codex_network_env()       ← CODEX_CONFIG (Codex only, when BUZZ_RELAY_URL set)
/// ```
///
/// Later layers override earlier ones on key collision (except default_agent_env and
/// claude_settings which skip keys already present in config.env).
pub fn build_acp_agent_config(config: &AgentConfig) -> agent_client_protocol::AcpAgentConfig {
    use agent_client_protocol::AcpAgentConfig;

    let command = normalize_agent_command_identity(&config.command);
    let args = normalize_agent_args(&command, config.args.clone());

    let mut agent_config = AcpAgentConfig::new(&command).args(args);

    // ① Global config env vars (lowest priority)
    match load_global_agent_config() {
        Ok(global) => {
            for (k, v) in &global.env_vars {
                agent_config = agent_config.env(k, v);
            }

            // ④ model/provider → agent-specific env vars (with global fallback)
            let runtime = known_acp_runtime(&command);
            let effective_model = config.model.as_deref().or(global.model.as_deref());
            let effective_provider = config.provider.as_deref().or(global.provider.as_deref());
            if let Some(rt) = runtime {
                for (k, v) in runtime_metadata_env_vars(
                    rt.model_env_var,
                    rt.provider_env_var,
                    rt.provider_locked,
                    effective_model,
                    effective_provider,
                ) {
                    agent_config = agent_config.env(&k, &v);
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load global agent config — skipping global env injection");

            // ④ Per-agent model/provider still work even without global config
            let runtime = known_acp_runtime(&command);
            if let Some(rt) = runtime {
                for (k, v) in runtime_metadata_env_vars(
                    rt.model_env_var,
                    rt.provider_env_var,
                    rt.provider_locked,
                    config.model.as_deref(),
                    config.provider.as_deref(),
                ) {
                    agent_config = agent_config.env(&k, &v);
                }
            }
        }
    }

    // ② Per-agent env map
    for (k, v) in &config.env {
        agent_config = agent_config.env(k, v);
    }

    // ③ Form fields → env vars
    for (k, v) in agent_config_to_env(config) {
        agent_config = agent_config.env(&k, &v);
    }

    // ⑤ Default env vars for known agents (skip if already in config.env)
    for &(key, value) in default_agent_env(&command) {
        if !config.env.contains_key(key) {
            agent_config = agent_config.env(key, value);
        }
    }

    // ⑥ Claude settings env (skip if already in config.env)
    if command == "claude" || command == "claude-code" || command == "claude-agent-acp" {
        let claude_env = read_claude_settings_env();
        for (k, v) in &claude_env {
            if !config.env.contains_key(k) {
                agent_config = agent_config.env(k, v);
            }
        }
    }

    // ⑦ Codex network config (only when BUZZ_RELAY_URL is set)
    if let Ok(relay_url) = std::env::var("BUZZ_RELAY_URL") {
        if let Some((k, v)) = codex_network_env(&command, &relay_url) {
            agent_config = agent_config.env(&k, &v);
        }
    }

    agent_config
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
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
///
/// 查找顺序：
/// 1. 旧路径: `~/.config/ergatai/agents/{name}.json`
/// 2. 新路径: `~/.config/ergatai/agents/{name}/settings.json` (hosted agent)
pub fn get_agent_config(name: &str) -> ErgataiResult<AgentConfig> {
    // 1. Try legacy path first (backward compatibility)
    let config_path = get_config_path(name)?;
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        let mut config: AgentConfig = serde_json::from_str(&content)?;
        normalize_agent_config(&mut config);
        return Ok(config);
    }

    // 2. Try hosted agent path
    match crate::agent::hosted_config::load_hosted_agent(name) {
        Ok(hosted_config) => {
            let mut config = crate::agent::hosted_config::to_agent_config(&hosted_config)
                .map_err(|e| ErgataiError::AgentNotFound(format!(
                    "Failed to convert hosted agent config for '{}': {}", name, e
                )))?;
            normalize_agent_config(&mut config);
            Ok(config)
        }
        Err(_) => {
            Err(ErgataiError::AgentNotFound(format!("Agent config not found: {}", name)))
        }
    }
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

pub fn get_config_path(name: &str) -> ErgataiResult<PathBuf> {
    // Prevent path traversal: reject names with separators or ".."
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name == "."
    {
        return Err(ErgataiError::InvalidArgument(format!(
            "Invalid agent config name (path traversal rejected): {:?}", name
        )));
    }
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

    // ── agent_config_to_env ──

    fn make_config(command: &str) -> AgentConfig {
        AgentConfig {
            name: "test".to_string(),
            command: command.to_string(),
            args: vec![],
            env: std::collections::HashMap::new(),
            display_name: None,
            base_url: None,
            model: None,
            provider: None,
            api_key: None,
            proxy: None,
            persona: None,
            agent_type: None,
            avatar: None,
        }
    }

    #[test]
    fn test_agent_config_to_env_claude() {
        let mut config = make_config("claude-agent-acp");
        config.api_key = Some("sk-ant-test".to_string());
        config.base_url = Some("https://api.example.com".to_string());

        let env = agent_config_to_env(&config);
        // Claude has api_key_env_var=ANTHROPIC_API_KEY, base_url_env_var=ANTHROPIC_BASE_URL
        assert!(env.contains(&("ANTHROPIC_API_KEY".to_string(), "sk-ant-test".to_string())));
        assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "https://api.example.com".to_string())));
        // NOTE: model/provider injection moved to build_acp_agent_config via runtime_metadata_env_vars
    }

    #[test]
    fn test_agent_config_to_env_codex() {
        let mut config = make_config("codex");
        config.api_key = Some("sk-openai-test".to_string());
        config.base_url = Some("https://proxy.example.com".to_string());

        let env = agent_config_to_env(&config);
        // Codex has api_key_env_var=OPENAI_API_KEY, base_url_env_var=OPENAI_BASE_URL
        assert!(env.contains(&("OPENAI_API_KEY".to_string(), "sk-openai-test".to_string())));
        assert!(env.contains(&("OPENAI_BASE_URL".to_string(), "https://proxy.example.com".to_string())));
    }

    #[test]
    fn test_agent_config_to_env_goose() {
        let mut config = make_config("goose");
        config.api_key = Some("key".to_string());
        config.base_url = Some("https://goose-api.example.com".to_string());

        let env = agent_config_to_env(&config);
        // Goose has api_key_env_var=OPENAI_API_KEY, base_url_env_var=GOOSE_API_BASE
        assert!(env.contains(&("OPENAI_API_KEY".to_string(), "key".to_string())));
        assert!(env.contains(&("GOOSE_API_BASE".to_string(), "https://goose-api.example.com".to_string())));
        // NOTE: model/provider injection moved to build_acp_agent_config via runtime_metadata_env_vars
    }

    #[test]
    fn test_agent_config_to_env_proxy() {
        let mut config = make_config("custom-agent");
        config.proxy = Some("http://proxy:8080".to_string());

        let env = agent_config_to_env(&config);
        assert!(env.contains(&("HTTP_PROXY".to_string(), "http://proxy:8080".to_string())));
        assert!(env.contains(&("HTTPS_PROXY".to_string(), "http://proxy:8080".to_string())));
    }

    #[test]
    fn test_agent_config_to_env_persona() {
        let mut config = make_config("custom-agent");
        config.persona = Some("You are an expert reviewer".to_string());

        let env = agent_config_to_env(&config);
        assert!(env.contains(&("ERGATAI_PERSONA".to_string(), "You are an expert reviewer".to_string())));
    }

    #[test]
    fn test_agent_config_to_env_empty_values_skipped() {
        let mut config = make_config("claude-code");
        config.api_key = Some("".to_string());
        config.base_url = Some("".to_string());

        let env = agent_config_to_env(&config);
        assert!(env.is_empty());
    }

    #[test]
    fn test_agent_config_to_env_none_values_skipped() {
        let config = make_config("claude-code");
        let env = agent_config_to_env(&config);
        assert!(env.is_empty());
    }

    #[test]
    fn test_agent_config_to_env_unknown_agent() {
        let mut config = make_config("my-custom-agent");
        config.api_key = Some("key123".to_string());
        config.base_url = Some("https://custom.api".to_string());

        let env = agent_config_to_env(&config);
        // Unknown agent falls back to generic env var names for api_key/base_url
        assert!(env.contains(&("API_KEY".to_string(), "key123".to_string())));
        assert!(env.contains(&("BASE_URL".to_string(), "https://custom.api".to_string())));
    }
}
