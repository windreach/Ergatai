//! Sensitive path detection for file access control.
//!
//! Detects sensitive files and directories that require ADMIN permission.
//! System defaults + project-level configuration.

use glob::Pattern;
use std::path::Path;
use tracing::warn;

/// System default sensitive path patterns
/// These always require ADMIN permission
const SYSTEM_SENSITIVE_PATTERNS: &[&str] = &[
    // Environment files (may contain secrets)
    ".env",
    ".env*",
    ".env.*",
    "**/.env",
    "**/.env.*",
    // Git internal files
    ".git/**",
    ".gitignore",
    ".gitattributes",
    // Credentials and keys
    "credentials/**",
    "**/credentials/**",
    "*.key",
    "*.pem",
    "*.p12",
    "*.pfx",
    "*.jks",
    // SSH keys
    "id_rsa*",
    "id_dsa*",
    "id_ecdsa*",
    "id_ed25519*",
    "**/.ssh/**",
    // AWS credentials
    "**/.aws/credentials",
    "**/.aws/config",
    // GCP credentials
    "**/service-account*.json",
    // Database files
    "*.sqlite",
    "*.db",
    // Certificate files
    "*.crt",
    "*.cer",
    // Private keys (generic)
    "*private*key*",
    "*secret*",
];

/// Check if a file path is sensitive and requires ADMIN permission
///
/// # Arguments
/// * `file_path` - The file path to check (relative to project root)
///
/// # Returns
/// `true` if the path is sensitive and requires ADMIN permission
pub fn is_sensitive_path(file_path: &str) -> bool {
    // Normalize path separators for cross-platform compatibility
    let normalized_path = file_path.replace('\\', "/");

    for pattern_str in SYSTEM_SENSITIVE_PATTERNS {
        match Pattern::new(pattern_str) {
            Ok(pattern) => {
                if pattern.matches(&normalized_path) {
                    warn!(
                        file_path = file_path,
                        pattern = pattern_str,
                        "Sensitive path detected, ADMIN permission required"
                    );
                    return true;
                }
            }
            Err(e) => {
                warn!(
                    pattern = pattern_str,
                    error = %e,
                    "Failed to parse sensitive path pattern"
                );
            }
        }
    }

    false
}

/// Check if a file path is within a sensitive directory
///
/// This is a stricter check that looks at directory components.
///
/// # Arguments
/// * `file_path` - The file path to check
///
/// # Returns
/// `true` if the path is within a sensitive directory
pub fn is_in_sensitive_directory(file_path: &str) -> bool {
    let path = Path::new(file_path);

    // Check each component of the path
    for component in path.components() {
        let component_str = component.as_os_str().to_string_lossy();

        // Check for sensitive directory names
        if component_str.starts_with('.') && component_str != "." && component_str != ".." {
            // Hidden directories (except . and ..)
            if component_str == ".git"
                || component_str == ".env"
                || component_str == ".ssh"
                || component_str == ".aws"
            {
                return true;
            }
        }

        // Check for credentials directories
        if component_str == "credentials" || component_str == "secrets" {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_files() {
        assert!(is_sensitive_path(".env"));
        assert!(is_sensitive_path(".env.local"));
        assert!(is_sensitive_path(".env.production"));
        assert!(is_sensitive_path("config/.env"));
    }

    #[test]
    fn test_git_files() {
        assert!(is_sensitive_path(".git/config"));
        assert!(is_sensitive_path(".git/HEAD"));
        assert!(is_sensitive_path(".git/objects/abc123"));
    }

    #[test]
    fn test_key_files() {
        assert!(is_sensitive_path("server.key"));
        assert!(is_sensitive_path("cert.pem"));
        assert!(is_sensitive_path("keys/private.key"));
    }

    #[test]
    fn test_credentials() {
        assert!(is_sensitive_path("credentials/aws.json"));
        assert!(is_sensitive_path("config/credentials/db.json"));
    }

    #[test]
    fn test_ssh_keys() {
        assert!(is_sensitive_path("id_rsa"));
        assert!(is_sensitive_path("id_rsa.pub"));
        assert!(is_sensitive_path(".ssh/id_ed25519"));
        assert!(is_sensitive_path(".ssh/config"));
    }

    #[test]
    fn test_non_sensitive_paths() {
        assert!(!is_sensitive_path("src/main.rs"));
        assert!(!is_sensitive_path("README.md"));
        assert!(!is_sensitive_path("package.json"));
        assert!(!is_sensitive_path("src/config/settings.ts"));
    }

    #[test]
    fn test_sensitive_directory() {
        assert!(is_in_sensitive_directory(".git/config"));
        assert!(is_in_sensitive_directory(".ssh/id_rsa"));
        assert!(is_in_sensitive_directory("credentials/db.json"));
        assert!(is_in_sensitive_directory("secrets/api-key.txt"));
    }

    #[test]
    fn test_non_sensitive_directory() {
        assert!(!is_in_sensitive_directory("src/main.rs"));
        assert!(!is_in_sensitive_directory("config/settings.json"));
    }
}
