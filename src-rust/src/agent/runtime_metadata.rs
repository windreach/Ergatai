//! Runtime metadata for known ACP agents.
//!
//! Each `KnownAcpRuntime` defines how to detect, configure, and launch a specific agent.
//! This metadata is hardcoded for the most popular agents and used by the discovery system.

use std::collections::HashMap;

/// Static metadata for a known ACP runtime.
pub struct KnownAcpRuntime {
    pub id: &'static str,
    pub label: &'static str,
    pub commands: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub avatar_file: &'static str,
    pub underlying_cli: Option<&'static str>,
    pub model_env_var: Option<&'static str>,
    pub provider_env_var: Option<&'static str>,
    pub provider_locked: bool,
    pub default_env: &'static [(&'static str, &'static str)],
    pub config_file_path: Option<&'static str>,
    pub install_instructions_url: &'static str,
    pub install_hint: &'static str,
    pub install_command: Option<&'static str>,  // Shell command to install this runtime
    pub login_hint: Option<&'static str>,
    pub auth_probe_args: Option<&'static [&'static str]>,
}

// Avatar filenames (bundled in resources/agent-icons/)
const GOOSE_AVATAR: &str = "goose.png";
const CLAUDE_CODE_AVATAR: &str = "claude-code.png";
const CODEX_AVATAR: &str = "codex.png";
const HERMES_AVATAR: &str = "hermes.png";

/// Known ACP runtimes with their metadata.
pub static KNOWN_ACP_RUNTIMES: &[KnownAcpRuntime] = &[
    KnownAcpRuntime {
        id: "goose",
        label: "Goose",
        commands: &["goose"],
        aliases: &[],
        avatar_file: GOOSE_AVATAR,
        underlying_cli: Some("goose"),
        model_env_var: Some("GOOSE_MODEL"),
        provider_env_var: Some("GOOSE_PROVIDER"),
        provider_locked: false,
        default_env: &[("GOOSE_MODE", "auto")],
        config_file_path: Some("~/.config/goose/config.yaml"),
        install_instructions_url: "https://goose-docs.ai/docs/getting-started/installation/",
        install_hint: "Install Goose CLI from https://goose-docs.ai",
        install_command: Some("npm install -g @block/goose"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "claude",
        label: "Claude Code",
        commands: &["claude-agent-acp", "claude-code-acp"],
        aliases: &["claude-code", "claudecode"],
        avatar_file: CLAUDE_CODE_AVATAR,
        underlying_cli: Some("claude"),
        model_env_var: None,
        provider_env_var: None,
        provider_locked: true, // Claude only supports Anthropic
        default_env: &[],
        config_file_path: Some("~/.claude/settings.json"),
        install_instructions_url: "https://code.claude.com/docs/en/getting-started",
        install_hint: "Install Claude Code CLI from https://claude.ai/install.sh",
        install_command: Some("npm install -g @anthropic-ai/claude-code"),
        login_hint: Some("Run `claude auth login` to authenticate."),
        auth_probe_args: Some(&["claude", "auth", "status"]),
    },
    KnownAcpRuntime {
        id: "codex",
        label: "Codex",
        commands: &["codex-acp"],
        aliases: &[],
        avatar_file: CODEX_AVATAR,
        underlying_cli: Some("codex"),
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: Some("~/.codex/config.toml"),
        install_instructions_url: "https://developers.openai.com/codex/cli/",
        install_hint: "Install Codex CLI from https://chatgpt.com/codex/install.sh",
        install_command: Some("npm install -g @openai/codex"),
        login_hint: Some("Run `codex login` to authenticate."),
        auth_probe_args: Some(&["codex", "login", "status"]),
    },
    KnownAcpRuntime {
        id: "hermes",
        label: "Hermes",
        commands: &["hermes-acp", "hermes"],
        aliases: &[],
        avatar_file: HERMES_AVATAR,
        underlying_cli: Some("hermes"),
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[("HERMES_ACP_SKIP_CONFIGURED_MCP", "1")],
        config_file_path: None,
        install_instructions_url: "https://hermes-agent.nousresearch.com",
        install_hint: "Install Hermes from https://hermes-agent.nousresearch.com",
        install_command: Some("pip install hermes-agent"),
        login_hint: None,
        auth_probe_args: None,
    },
    // Tier 2: Preset agents (simpler metadata, no env var mappings)
    KnownAcpRuntime {
        id: "devin",
        label: "Devin",
        commands: &["devin"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://docs.devin.ai/cli",
        install_hint: "Install Devin CLI from https://docs.devin.ai/cli",
        install_command: Some("npm install -g @devin/cli"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "cursor",
        label: "Cursor",
        commands: &["cursor-agent"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://cursor.com/downloads",
        install_hint: "Install Cursor from https://cursor.com/downloads",
        install_command: None,  // Desktop app, manual download
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "omp",
        label: "Oh My Pi",
        commands: &["omp"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://omp.sh/",
        install_hint: "Install Oh My Pi from https://omp.sh/",
        install_command: None,  // Manual install via web UI
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "grok",
        label: "Grok Build",
        commands: &["grok"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://build.x.ai/docs",
        install_hint: "Install Grok Build from https://build.x.ai/docs",
        install_command: Some("npm install -g @x-ai/grok-build"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "opencode",
        label: "OpenCode",
        commands: &["opencode"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://opencode.ai/docs",
        install_hint: "Install OpenCode from https://opencode.ai/docs",
        install_command: Some("npm install -g @opencode/cli"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "kimi",
        label: "Kimi Code",
        commands: &["kimi"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://kimi.ai/download",
        install_hint: "Install Kimi Code from https://kimi.ai/download",
        install_command: Some("npm install -g @kimi/code"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "amp",
        label: "Amp",
        commands: &["amp-acp"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: Some("amp"),
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://github.com/tao12345666333/amp-acp",
        install_hint: "Install Amp ACP adapter from https://github.com/tao12345666333/amp-acp",
        install_command: Some("npm install -g @tao12345666333/amp-acp"),
        login_hint: None,
        auth_probe_args: None,
    },
    KnownAcpRuntime {
        id: "openclaw",
        label: "OpenClaw",
        commands: &["openclaw"],
        aliases: &[],
        avatar_file: "",
        underlying_cli: None,
        model_env_var: None,
        provider_env_var: None,
        provider_locked: false,
        default_env: &[],
        config_file_path: None,
        install_instructions_url: "https://docs.openclaw.ai/start/getting-started",
        install_hint: "Install OpenClaw from https://docs.openclaw.ai/start/getting-started",
        install_command: Some("pip install openclaw"),
        login_hint: None,
        auth_probe_args: None,
    },
];

/// Find a known runtime by command name (normalized).
pub fn known_acp_runtime(command: &str) -> Option<&'static KnownAcpRuntime> {
    let normalized = normalize_command_identity(command);

    KNOWN_ACP_RUNTIMES.iter().find(|runtime| {
        normalized == runtime.id
            || runtime.commands.iter().any(|cmd| normalized == normalize_command_identity(cmd))
            || runtime.aliases.iter().any(|alias| normalized == *alias)
    })
}

/// Find a known runtime by exact ID.
pub fn known_acp_runtime_exact(id: &str) -> Option<&'static KnownAcpRuntime> {
    KNOWN_ACP_RUNTIMES.iter().find(|p| p.id == id)
}

/// Normalize a command name to a canonical form.
///
/// - Strips path components (e.g., `/usr/local/bin/goose` → `goose`)
/// - Lowercases
/// - Replaces `_` and spaces with `-`
/// - Strips `.exe`, `.cmd`, `.bat` suffixes on Windows
pub fn normalize_command_identity(command: &str) -> String {
    let normalized = command.trim().replace('\\', "/");
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    let lower = basename
        .chars()
        .map(|c| match c {
            ' ' | '_' => '-',
            _ => c.to_ascii_lowercase(),
        })
        .collect::<String>();
    let lower = lower.strip_suffix(".exe").unwrap_or(&lower).to_string();

    #[cfg(windows)]
    {
        if let Some(suffix) = std::env::consts::EXE_SUFFIX.strip_prefix('.') {
            return lower
                .strip_suffix(&format!(".{suffix}"))
                .unwrap_or(&lower)
                .to_string();
        }
        if !std::env::consts::EXE_SUFFIX.is_empty() {
            return lower
                .strip_suffix(std::env::consts::EXE_SUFFIX)
                .unwrap_or(&lower)
                .to_string();
        }
    }

    lower
}

/// Get default agent args for a command (if any).
pub fn default_agent_args(command: &str) -> Option<Vec<String>> {
    let normalized = normalize_command_identity(command);
    match normalized.as_str() {
        "goose" => Some(vec!["acp".to_string()]),
        "claude-agent-acp" | "claude-code-acp" | "claude-code" => Some(vec![]),
        "codex-acp" | "codex" => Some(vec![]),
        _ => None,
    }
}

/// Get default environment variables for a command (if any).
pub fn default_agent_env(command: &str) -> Option<HashMap<String, String>> {
    let runtime = known_acp_runtime(command)?;
    let mut env = HashMap::new();
    for (key, value) in runtime.default_env {
        env.insert(key.to_string(), value.to_string());
    }
    Some(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_command_identity() {
        assert_eq!(normalize_command_identity("goose"), "goose");
        assert_eq!(normalize_command_identity("Goose"), "goose");
        assert_eq!(normalize_command_identity("Claude_Code"), "claude-code");
        assert_eq!(normalize_command_identity("/usr/local/bin/goose"), "goose");
        assert_eq!(normalize_command_identity("claude-agent-acp"), "claude-agent-acp");
    }

    #[test]
    fn test_known_acp_runtime() {
        assert!(known_acp_runtime("goose").is_some());
        assert!(known_acp_runtime("Goose").is_some());
        assert!(known_acp_runtime("claude-code").is_some());
        assert!(known_acp_runtime("claude-agent-acp").is_some());
        assert!(known_acp_runtime("unknown").is_none());
    }

    #[test]
    fn test_default_agent_args() {
        assert_eq!(default_agent_args("goose"), Some(vec!["acp".to_string()]));
        assert_eq!(default_agent_args("claude"), Some(vec![]));
        assert_eq!(default_agent_args("unknown"), None);
    }

    #[test]
    fn test_default_agent_env() {
        let goose_env = default_agent_env("goose").unwrap();
        assert_eq!(goose_env.get("GOOSE_MODE"), Some(&"auto".to_string()));

        let hermes_env = default_agent_env("hermes").unwrap();
        assert_eq!(hermes_env.get("HERMES_ACP_SKIP_CONFIGURED_MCP"), Some(&"1".to_string()));
    }
}
