//! ACP runtime installation logic.
//!
//! Executes predefined install commands from runtime metadata to install agents.

use std::process::Command;
use anyhow::{Result, Context};

/// Command whitelist prefixes (prevent command injection)
const ALLOWED_PREFIXES: &[&str] = &[
    "npm install",
    "cargo install",
    "brew install",
    "pip install",
];

/// Validate that an install command is safe to execute
fn validate_install_command(cmd: &str) -> Result<()> {
    if !ALLOWED_PREFIXES.iter().any(|prefix| cmd.starts_with(prefix)) {
        return Err(anyhow::anyhow!(
            "Install command not in whitelist: {}",
            cmd
        ));
    }
    Ok(())
}

/// Install a specific ACP runtime by executing its install command
pub fn install_acp_runtime(runtime_id: &str) -> Result<String> {
    let metadata = crate::agent::runtime_metadata::known_acp_runtime_exact(runtime_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown runtime: {}", runtime_id))?;

    let install_cmd = metadata.install_command
        .ok_or_else(|| anyhow::anyhow!("No install command for: {}", runtime_id))?;

    // Security check: command must be in whitelist
    validate_install_command(install_cmd)?;

    // Execute install command via shell
    let output = Command::new("sh")
        .arg("-c")
        .arg(install_cmd)
        .output()
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
        assert!(validate_install_command("npm install -g @block/goose").is_ok());
        assert!(validate_install_command("pip install hermes-agent").is_ok());
        assert!(validate_install_command("brew install something").is_ok());
        assert!(validate_install_command("rm -rf /").is_err());
        assert!(validate_install_command("curl | sh").is_err());
    }

    #[test]
    fn test_install_unknown_runtime() {
        let result = install_acp_runtime("nonexistent-runtime");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown runtime"));
    }
}
