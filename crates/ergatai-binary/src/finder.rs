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
