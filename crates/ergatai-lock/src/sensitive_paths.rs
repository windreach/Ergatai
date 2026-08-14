//! Sensitive path detection for file access control.
//!
//! Detects sensitive files and directories that require ADMIN permission.
//! System defaults + project-level configuration.

use glob::Pattern;
use std::path::Path;
use std::sync::LazyLock;

/// Check if a path is a symbolic link.
///
/// Symlinks can be used to bypass path validation and access sensitive files
/// outside the allowed directory tree.
///
/// # Arguments
/// * `path` - The path to check
///
/// # Returns
/// `Ok(())` if the path is safe (not a symlink), `Err` with description if it's a symlink.
pub fn check_symlink(path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                Err(format!(
                    "Symbolic link not allowed: {}. Symlinks can bypass path validation.",
                    path.display()
                ))
            } else {
                Ok(())
            }
        }
        Err(e) => {
            // If we can't read metadata, treat as error
            Err(format!("Cannot read metadata for {}: {}", path.display(), e))
        }
    }
}

/// System default sensitive path patterns
/// These always require ADMIN permission
const SYSTEM_SENSITIVE_PATTERNS: &[&str] = &[
    // Environment files (may contain secrets)
    ".env",
    ".env*",
    "**/.env*",
    ".env.*",
    "**/.env",
    "**/.env.*",
    "*.env",    // Files ending with .env (e.g., prod.env, development.env)
    "**/*.env", // Nested files ending with .env
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

/// Pre-compiled sensitive path patterns (compiled once at first access)
static COMPILED_PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    SYSTEM_SENSITIVE_PATTERNS
        .iter()
        .filter_map(|p| {
            Pattern::new(p)
                .map_err(|e| {
                    tracing::warn!(pattern = p, error = %e, "Failed to parse sensitive path pattern")
                })
                .ok()
        })
        .collect()
});

/// Check if a file path is sensitive and requires ADMIN permission
///
/// # Arguments
/// * `file_path` - The file path to check (relative to project root)
///
/// # Returns
/// `true` if the path is sensitive and requires ADMIN permission
///
/// # Security
/// Rejects absolute paths and paths containing `..` to prevent bypass attempts.
pub fn is_sensitive_path(file_path: &str) -> bool {
    let path = Path::new(file_path);

    // Reject absolute paths - they could bypass relative-path patterns
    if path.is_absolute() {
        tracing::warn!(
            path = file_path,
            "Absolute path rejected in is_sensitive_path"
        );
        return true; // Treat as sensitive to be safe
    }

    // Reject paths with parent directory references to prevent traversal
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            tracing::warn!(
                path = file_path,
                "Path traversal (..) rejected in is_sensitive_path"
            );
            return true; // Treat as sensitive to be safe
        }
    }

    // Normalize path separators for cross-platform compatibility
    let normalized_path = file_path.replace('\\', "/");

    // Use pre-compiled patterns (LazyLock ensures single compilation)
    COMPILED_PATTERNS
        .iter()
        .any(|pattern| pattern.matches(&normalized_path))
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
///
/// # Security
/// Rejects absolute paths and paths containing `..` to prevent bypass attempts.
pub fn is_in_sensitive_directory(file_path: &str) -> bool {
    let path = Path::new(file_path);

    // Reject absolute paths - they could bypass relative-path checks
    if path.is_absolute() {
        tracing::warn!(
            path = file_path,
            "Absolute path rejected in is_in_sensitive_directory"
        );
        return true; // Treat as sensitive to be safe
    }

    // Check each component of the path
    for component in path.components() {
        // Reject parent directory references
        if matches!(component, std::path::Component::ParentDir) {
            tracing::warn!(
                path = file_path,
                "Path traversal (..) rejected in is_in_sensitive_directory"
            );
            return true; // Treat as sensitive to be safe
        }

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

    #[test]
    fn test_aws_credentials() {
        assert!(is_sensitive_path(".aws/credentials"));
        assert!(is_sensitive_path("config/.aws/credentials"));
        assert!(is_sensitive_path(".aws/config"));
        assert!(is_in_sensitive_directory(".aws/credentials"));
    }

    #[test]
    fn test_gcp_service_account() {
        assert!(is_sensitive_path("service-account.json"));
        assert!(is_sensitive_path("gcp/service-account-prod.json"));
    }

    #[test]
    fn test_certificate_files() {
        assert!(is_sensitive_path("server.crt"));
        assert!(is_sensitive_path("ca.cer"));
        assert!(is_sensitive_path("keystore.p12"));
        assert!(is_sensitive_path("bundle.pfx"));
        assert!(is_sensitive_path("truststore.jks"));
    }

    #[test]
    fn test_database_files() {
        assert!(is_sensitive_path("app.sqlite"));
        assert!(is_sensitive_path("data/users.db"));
    }

    #[test]
    fn test_private_key_and_secret_patterns() {
        assert!(is_sensitive_path("my-private-key.pem"));
        assert!(is_sensitive_path("config/secret.json"));
        assert!(is_sensitive_path("app-secrets.yaml"));
    }

    #[test]
    fn test_windows_path_separator_normalized() {
        // Backslashes should be normalized to forward slashes before matching
        assert!(is_sensitive_path(".aws\\credentials"));
        assert!(is_sensitive_path(".ssh\\id_rsa"));
    }

    #[test]
    fn test_non_sensitive_hidden_directory() {
        // .vscode, .idea, etc. are not in the sensitive list
        assert!(!is_in_sensitive_directory(".vscode/settings.json"));
        assert!(!is_in_sensitive_directory(".idea/workspace.xml"));
    }

    #[test]
    fn test_sensitive_directory_deeply_nested() {
        assert!(is_in_sensitive_directory("project/nested/.ssh/id_rsa"));
        assert!(is_in_sensitive_directory("a/b/c/credentials/db.json"));
        assert!(is_in_sensitive_directory("a/b/secrets/key.txt"));
    }

    #[test]
    fn test_empty_and_root_paths() {
        // Empty paths shouldn't be detected as sensitive
        assert!(!is_sensitive_path(""));
        assert!(!is_in_sensitive_directory(""));
        // Absolute paths are rejected as sensitive for security
        assert!(is_in_sensitive_directory("/"));
        assert!(is_sensitive_path("/etc/passwd"));
    }

    #[test]
    fn test_check_symlink_nonexistent() {
        // Non-existent path should return error (can't read metadata)
        let result = check_symlink(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
    }

    #[test]
    fn test_check_symlink_regular_file() {
        // Create a temporary regular file and verify it passes
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("ergatai_test_regular_file.txt");
        std::fs::write(&temp_file, "test").unwrap();
        let result = check_symlink(&temp_file);
        assert!(result.is_ok());
        std::fs::remove_file(&temp_file).ok();
    }
}
