//! Binary locator with multi-layer search strategy
//!
//! Search order:
//! 1. Environment variable override (e.g., `ERGATAI_RMUX_BINARY`)
//! 2. Bundled resources (downloaded by build.rs at compile time)
//! 3. Sibling directory (next to the executable)
//! 4. System PATH (development fallback)

use ergatai_error::{ErgataiError, ErgataiResult};
use std::path::{Path, PathBuf};

/// Binary file locator with environment override, bundled resources, and PATH fallback
pub struct BinaryLocator {
    /// Binary name (e.g., "nats-server", "rmux-daemon")
    pub name: &'static str,
    /// Environment variable override name (e.g., "ERGATAI_NATS_BINARY")
    pub env_override: Option<&'static str>,
    /// Resource subdirectory pattern (e.g., "nats-server-{platform}")
    pub resource_subdir_pattern: Option<&'static str>,
}

impl BinaryLocator {
    /// Multi-layer search: env var → bundled resources → sibling → system PATH
    pub fn find(&self) -> ErgataiResult<PathBuf> {
        // 1. Environment variable override
        if let Some(env_name) = self.env_override {
            if let Ok(path) = std::env::var(env_name) {
                let path = PathBuf::from(&path);
                if path.exists() {
                    tracing::debug!(
                        name = self.name,
                        path = %path.display(),
                        "found via environment variable"
                    );
                    return Ok(path);
                }
                tracing::warn!(
                    env = env_name,
                    path = %path.display(),
                    "env var points to non-existent file"
                );
            }
        }

        // 2. Bundled resources (from build.rs download)
        if let Some(path) = self.find_bundled() {
            tracing::debug!(
                name = self.name,
                path = %path.display(),
                "found in bundled resources"
            );
            return Ok(path);
        }

        // 3. Sibling directory (next to executable)
        if let Some(path) = self.find_sibling() {
            tracing::debug!(
                name = self.name,
                path = %path.display(),
                "found in sibling directory"
            );
            return Ok(path);
        }

        // 4. System PATH (fallback)
        if let Some(path) = self.find_on_path() {
            tracing::warn!(
                name = self.name,
                path = %path.display(),
                "using {} from system PATH (bundled version not found)",
                self.name
            );
            return Ok(path);
        }

        Err(ErgataiError::internal(format!(
            "{} binary not found. Set {} or install {}",
            self.name,
            self.env_override.unwrap_or("(unset)"),
            self.name
        )))
    }

    /// Look for binary in bundled resources directory.
    /// Supports both simple platform names (linux, darwin) and
    /// architecture-specific names (linux-x86_64, darwin-arm64).
    fn find_bundled(&self) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let binary_name = binary_file_name(self.name);

        // Check multiple possible locations
        let candidates = self.bundled_candidate_paths(&exe_dir, &binary_name);

        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Also check relative to CARGO_MANIFEST_DIR for development
        if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
            let manifest_path = PathBuf::from(manifest_dir);
            let candidates = self.bundled_candidate_paths(&manifest_path, &binary_name);
            for candidate in candidates {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    /// Generate candidate paths for bundled binary lookup
    fn bundled_candidate_paths(&self, base_dir: &Path, binary_name: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        let platform = platform_dir_name();
        let platform_arch = platform_arch_dir_name();

        // Pattern-based path (if specified)
        if let Some(pattern) = self.resource_subdir_pattern {
            // Try with arch-specific platform name first
            let dir = pattern.replace("{platform}", platform_arch);
            candidates.push(base_dir.join("resources").join(&dir).join(binary_name));
            // Also check inside extracted archive directory (e.g., rmux-0.10.0-linux-x86_64/bin/rmux)
            candidates.push(
                base_dir
                    .join("resources")
                    .join(&dir)
                    .join("bin")
                    .join(binary_name),
            );

            // Try with simple platform name
            let dir = pattern.replace("{platform}", platform);
            candidates.push(base_dir.join("resources").join(&dir).join(binary_name));
            candidates.push(
                base_dir
                    .join("resources")
                    .join(&dir)
                    .join("bin")
                    .join(binary_name),
            );
        }

        // Direct platform paths
        candidates.push(
            base_dir
                .join("resources")
                .join(platform_arch)
                .join(binary_name),
        );
        candidates.push(
            base_dir
                .join("resources")
                .join(platform_arch)
                .join("bin")
                .join(binary_name),
        );
        candidates.push(base_dir.join("resources").join(platform).join(binary_name));
        candidates.push(
            base_dir
                .join("resources")
                .join(platform)
                .join("bin")
                .join(binary_name),
        );

        // Search for versioned directories (e.g., rmux-0.10.0-linux-x86_64)
        let resources_dir = base_dir.join("resources").join(platform_arch);
        if resources_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&resources_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        // Check if this directory has a bin/ subdirectory with our binary
                        let bin_path = path.join("bin").join(binary_name);
                        if bin_path.exists() {
                            candidates.push(bin_path);
                        }
                    }
                }
            }
        }

        candidates
    }

    fn find_sibling(&self) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let binary_name = binary_file_name(self.name);
        let candidate = exe_dir.join(&binary_name);
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    }

    fn find_on_path(&self) -> Option<PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(binary_file_name(self.name));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
}

/// Simple platform name (without architecture)
fn platform_dir_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

/// Platform name with architecture (matches build.rs output)
fn platform_arch_dir_name() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "darwin-arm64"
        } else {
            "darwin-x86_64"
        }
    } else if cfg!(target_os = "windows") {
        "win32-x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-x86_64"
    }
}

fn binary_file_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        // Ensure .exe extension on Windows
        if name.ends_with(".exe") {
            name.to_string()
        } else {
            format!("{}.exe", name)
        }
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_platform_dir_name_returns_non_empty() {
        let name = platform_dir_name();
        assert!(!name.is_empty());
        // On Linux, it should be "linux"; on macOS, "darwin"; on Windows, "win32"
        assert!(
            name == "linux" || name == "darwin" || name == "win32",
            "unexpected platform: {}",
            name
        );
    }

    #[test]
    fn test_platform_arch_dir_name_returns_non_empty() {
        let name = platform_arch_dir_name();
        assert!(!name.is_empty());
        // Should contain the base platform
        assert!(
            name.contains("linux") || name.contains("darwin") || name.contains("win32"),
            "unexpected platform_arch: {}",
            name
        );
    }

    #[test]
    fn test_platform_arch_contains_platform() {
        let simple = platform_dir_name();
        let with_arch = platform_arch_dir_name();
        assert!(
            with_arch.starts_with(simple),
            "{} should start with {}",
            with_arch,
            simple
        );
    }

    #[test]
    fn test_binary_file_name_unix() {
        if cfg!(not(target_os = "windows")) {
            assert_eq!(binary_file_name("rmux"), "rmux");
            assert_eq!(binary_file_name("nats-server"), "nats-server");
        }
    }

    #[test]
    fn test_binary_file_name_windows() {
        if cfg!(target_os = "windows") {
            assert_eq!(binary_file_name("rmux"), "rmux.exe");
            // Already has .exe - should not double-add
            assert_eq!(binary_file_name("rmux.exe"), "rmux.exe");
        }
    }

    #[test]
    fn test_binary_locator_find_env_override_nonexistent_path() {
        // Set an env var pointing to a non-existent path; locator should skip it
        let env_name = "ERGATAI_TEST_BINARY_NONEXISTENT";
        std::env::set_var(env_name, "/nonexistent/path/to/binary");

        let locator = BinaryLocator {
            name: "nonexistent-binary",
            env_override: Some(env_name),
            resource_subdir_pattern: None,
        };

        // Should fail because env path doesn't exist, no fallbacks available
        let result = locator.find();
        assert!(result.is_err());

        std::env::remove_var(env_name);
    }

    #[test]
    fn test_binary_locator_find_env_override_valid_path() {
        let tmp = std::env::temp_dir().join("ergatai_test_binary");
        fs::write(&tmp, "fake binary").unwrap();

        let env_name = "ERGATAI_TEST_BINARY_VALID";
        std::env::set_var(env_name, tmp.to_str().unwrap());

        let locator = BinaryLocator {
            name: "test-binary",
            env_override: Some(env_name),
            resource_subdir_pattern: None,
        };

        let result = locator.find();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tmp);

        std::env::remove_var(env_name);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_binary_locator_find_no_env_no_fallback() {
        // A locator with no env, no bundled, no sibling - should fall back to PATH
        // If the binary doesn't exist on PATH, it should error
        let locator = BinaryLocator {
            name: "ergatai-nonexistent-binary-12345",
            env_override: None,
            resource_subdir_pattern: None,
        };
        let result = locator.find();
        // It might succeed if somehow on PATH, but most likely fails
        // We only assert the error message is reasonable
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                msg.contains("ergatai-nonexistent-binary-12345"),
                "error should mention binary name: {}",
                msg
            );
        }
    }

    #[test]
    fn test_bundled_candidate_paths_with_pattern() {
        let tmp = std::env::temp_dir().join("ergatai_test_bundled");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let locator = BinaryLocator {
            name: "rmux",
            env_override: None,
            resource_subdir_pattern: Some("rmux-{platform}"),
        };

        let candidates = locator.bundled_candidate_paths(&tmp, "rmux");
        // Should have multiple candidate paths
        assert!(!candidates.is_empty());

        // All paths should be under the resources directory
        for c in &candidates {
            assert!(
                c.starts_with(tmp.join("resources")),
                "{} should be under resources",
                c.display()
            );
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bundled_candidate_paths_without_pattern() {
        let tmp = std::env::temp_dir().join("ergatai_test_bundled_nopattern");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let locator = BinaryLocator {
            name: "nats-server",
            env_override: None,
            resource_subdir_pattern: None,
        };

        let candidates = locator.bundled_candidate_paths(&tmp, "nats-server");
        // Should still have direct platform paths
        assert!(!candidates.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_bundled_candidate_paths_finds_versioned_dir() {
        let tmp = std::env::temp_dir().join("ergatai_test_versioned");
        let _ = fs::remove_dir_all(&tmp);

        // Create a versioned directory structure
        let platform_arch = platform_arch_dir_name();
        let versioned_dir = tmp
            .join("resources")
            .join(platform_arch)
            .join("rmux-0.10.0");
        let bin_dir = versioned_dir.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let binary_path = bin_dir.join("rmux");
        fs::write(&binary_path, "fake").unwrap();

        let locator = BinaryLocator {
            name: "rmux",
            env_override: None,
            resource_subdir_pattern: None,
        };

        let candidates = locator.bundled_candidate_paths(&tmp, "rmux");
        // Should include the versioned binary path
        assert!(
            candidates.iter().any(|c| c == &binary_path),
            "candidates should include versioned path: {:?}",
            candidates
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_find_sibling_nonexistent() {
        let locator = BinaryLocator {
            name: "ergatai-definitely-not-a-real-binary",
            env_override: None,
            resource_subdir_pattern: None,
        };
        // find_sibling returns None when binary not next to executable
        // We can't easily control current_exe in tests, so just check it doesn't panic
        let _ = locator.find_sibling();
    }

    #[test]
    fn test_find_on_path_for_common_binary() {
        // Test find_on_path with a binary that's likely on PATH
        // This tests the mechanism, not the actual binary presence
        let locator = BinaryLocator {
            name: "ergatai-nonexistent-test-binary-xyz",
            env_override: None,
            resource_subdir_pattern: None,
        };
        let result = locator.find_on_path();
        // Should be None since binary doesn't exist
        assert!(result.is_none());
    }

    #[test]
    fn test_binary_locator_error_message_mentions_env_var() {
        let locator = BinaryLocator {
            name: "my-binary",
            env_override: Some("MY_BINARY_PATH"),
            resource_subdir_pattern: None,
        };
        let result = locator.find();
        if let Err(e) = result {
            let msg = e.to_string();
            assert!(
                msg.contains("MY_BINARY_PATH"),
                "error should mention env var: {}",
                msg
            );
        }
    }
}
