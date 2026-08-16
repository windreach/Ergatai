//! Binary locator with 3-layer search strategy

use std::path::PathBuf;
use ergatai_error::{ErgataiError, ErgataiResult};

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
    /// 3-layer search: env var → bundled resources → system PATH
    pub fn find(&self) -> ErgataiResult<PathBuf> {
        // 1. Environment variable override
        if let Some(env_name) = self.env_override {
            if let Ok(path) = std::env::var(env_name) {
                let path = PathBuf::from(path);
                if path.exists() {
                    return Ok(path);
                }
                tracing::warn!(
                    env = env_name,
                    path = %path.display(),
                    "env var points to non-existent file"
                );
            }
        }

        // 2. Bundled resources
        if let Some(path) = self.find_bundled() {
            return Ok(path);
        }

        // 3. Sibling directory
        if let Some(path) = self.find_sibling() {
            return Ok(path);
        }

        // 4. System PATH
        if let Some(path) = self.find_on_path() {
            tracing::warn!(
                name = self.name,
                "using {} from system PATH (not recommended for production)",
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

    fn find_bundled(&self) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let platform = platform_dir_name();
        let binary_name = binary_file_name(self.name);

        if let Some(pattern) = self.resource_subdir_pattern {
            let dir = pattern.replace("{platform}", platform);
            let candidate = exe_dir.join("resources").join(&dir).join(&binary_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        let candidate = exe_dir.join("resources").join(platform).join(&binary_name);
        if candidate.exists() {
            return Some(candidate);
        }

        None
    }

    fn find_sibling(&self) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let binary_name = binary_file_name(self.name);
        let candidate = exe_dir.join(&binary_name);
        if candidate.exists() {
            return Some(candidate);
        }
        None
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

fn platform_dir_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    }
}

fn binary_file_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}
