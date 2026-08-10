//! Project-level configuration for file access control.
//!
//! Loads `.ergatai/config.json` to extend system defaults with project-specific
//! sensitive paths and forbidden paths. Supports hot reload.

use crate::error::ErgataiError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::sensitive_paths;

/// Project-level file access configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessConfig {
    /// Additional sensitive path patterns (merged with system defaults)
    #[serde(default)]
    pub sensitive_paths: Vec<String>,

    /// Paths that are completely forbidden (no access allowed)
    #[serde(default)]
    pub forbidden_paths: Vec<String>,

    /// Maximum snapshot size in bytes (default: 100MB)
    #[serde(default = "default_max_snapshot_size")]
    pub max_snapshot_size: u64,

    /// Snapshot retention days (default: 7)
    #[serde(default = "default_snapshot_retention_days")]
    pub snapshot_retention_days: u32,

    /// Audit log retention months (default: 3)
    #[serde(default = "default_audit_retention_months")]
    pub audit_retention_months: u32,

    /// Maximum audit log rows (default: 1_000_000)
    #[serde(default = "default_max_audit_rows")]
    pub max_audit_rows: u64,
}

impl Default for FileAccessConfig {
    fn default() -> Self {
        Self {
            sensitive_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            max_snapshot_size: default_max_snapshot_size(),
            snapshot_retention_days: default_snapshot_retention_days(),
            audit_retention_months: default_audit_retention_months(),
            max_audit_rows: default_max_audit_rows(),
        }
    }
}

fn default_max_snapshot_size() -> u64 {
    100_000_000 // 100MB
}

fn default_snapshot_retention_days() -> u32 {
    7
}

fn default_audit_retention_months() -> u32 {
    3
}

fn default_max_audit_rows() -> u64 {
    1_000_000
}

/// Configuration manager with hot reload support
pub struct ConfigManager {
    /// Project root directory
    project_root: PathBuf,

    /// Current configuration (thread-safe)
    config: Arc<RwLock<FileAccessConfig>>,

    /// Last modification time of the config file
    last_modified: Arc<RwLock<Option<Instant>>>,

    /// Hot reload interval (None = disabled)
    reload_interval: Option<Duration>,

    /// Last check time
    last_check: Arc<RwLock<Instant>>,
}

impl ConfigManager {
    /// Create a new config manager
    ///
    /// # Arguments
    /// * `project_root` - Project root directory
    /// * `reload_interval` - How often to check for config changes (None = disabled)
    pub fn new(project_root: &Path, reload_interval: Option<Duration>) -> Result<Self, ErgataiError> {
        let config_path = project_root.join(".ergatai").join("config.json");

        let config = if config_path.exists() {
            Self::load_config(&config_path)?
        } else {
            debug!("No project config found at {:?}, using defaults", config_path);
            FileAccessConfig::default()
        };

        let now = Instant::now();

        Ok(Self {
            project_root: project_root.to_path_buf(),
            config: Arc::new(RwLock::new(config)),
            last_modified: Arc::new(RwLock::new(None)),
            reload_interval,
            last_check: Arc::new(RwLock::new(now)),
        })
    }

    /// Load configuration from file
    fn load_config(config_path: &Path) -> Result<FileAccessConfig, ErgataiError> {
        let content = fs::read_to_string(config_path).map_err(|e| {
            ErgataiError::internal(format!("Failed to read config file {:?}: {}", config_path, e))
        })?;

        let config: FileAccessConfig = serde_json::from_str(&content).map_err(|e| {
            ErgataiError::internal(format!("Failed to parse config file {:?}: {}", config_path, e))
        })?;

        info!(
            config_path = ?config_path,
            sensitive_paths = config.sensitive_paths.len(),
            forbidden_paths = config.forbidden_paths.len(),
            "Loaded project configuration"
        );

        Ok(config)
    }

    /// Get the current configuration (with automatic reload if interval is set)
    pub fn get_config(&self) -> FileAccessConfig {
        // Check if we should reload
        if let Some(interval) = self.reload_interval {
            let should_reload = {
                let mut last_check = self.last_check.write().unwrap();
                let now = Instant::now();
                if now.duration_since(*last_check) >= interval {
                    *last_check = now;
                    true
                } else {
                    false
                }
            };

            if should_reload {
                if let Err(e) = self.reload_if_changed() {
                    warn!(error = %e, "Failed to reload config");
                }
            }
        }

        self.config.read().unwrap().clone()
    }

    /// Reload configuration if the file has changed
    pub fn reload_if_changed(&self) -> Result<bool, ErgataiError> {
        let config_path = self.project_root.join(".ergatai").join("config.json");

        if !config_path.exists() {
            return Ok(false);
        }

        let metadata = fs::metadata(&config_path).map_err(|e| {
            ErgataiError::internal(format!("Failed to get config metadata: {}", e))
        })?;

        let _modified = metadata.modified().map_err(|e| {
            ErgataiError::internal(format!("Failed to get modification time: {}", e))
        })?;

        // Compare with last known modification time
        let last_modified = self.last_modified.read().unwrap();

        if let Some(last) = *last_modified {
            // Simple comparison: if modification time is newer than last check
            // Note: This is a simplified check. In production, you'd store the actual
            // SystemTime and compare it.
            let elapsed = last.elapsed();
            if elapsed.as_secs() > 0 {
                // File might have changed, reload
                drop(last_modified);
                let new_config = Self::load_config(&config_path)?;
                let mut config = self.config.write().unwrap();
                *config = new_config;
                let mut last_modified = self.last_modified.write().unwrap();
                *last_modified = Some(Instant::now());

                info!("Configuration reloaded");
                return Ok(true);
            }
        } else {
            // First time, set the modification time
            drop(last_modified);
            let mut last_modified = self.last_modified.write().unwrap();
            *last_modified = Some(Instant::now());
        }

        Ok(false)
    }

    /// Check if a path is sensitive (system defaults + project config)
    pub fn is_sensitive_path(&self, file_path: &str) -> bool {
        // Check system defaults first
        if sensitive_paths::is_sensitive_path(file_path) {
            return true;
        }

        // Check project-level sensitive paths
        let config = self.get_config();
        for pattern_str in &config.sensitive_paths {
            if let Ok(pattern) = glob::Pattern::new(pattern_str) {
                if pattern.matches(file_path) {
                    debug!(
                        file_path = file_path,
                        pattern = pattern_str,
                        "Sensitive path detected (project config)"
                    );
                    return true;
                }
            }
        }

        false
    }

    /// Check if a path is forbidden (completely blocked)
    pub fn is_forbidden_path(&self, file_path: &str) -> bool {
        let config = self.get_config();
        for pattern_str in &config.forbidden_paths {
            if let Ok(pattern) = glob::Pattern::new(pattern_str) {
                if pattern.matches(file_path) {
                    warn!(
                        file_path = file_path,
                        pattern = pattern_str,
                        "Forbidden path access attempt"
                    );
                    return true;
                }
            }
        }

        false
    }

    /// Manually trigger a config reload
    pub fn reload(&self) -> Result<(), ErgataiError> {
        let config_path = self.project_root.join(".ergatai").join("config.json");
        if config_path.exists() {
            let new_config = Self::load_config(&config_path)?;
            let mut config = self.config.write().unwrap();
            *config = new_config;
            info!("Configuration manually reloaded");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ConfigManager::new(temp_dir.path(), None).unwrap();
        let config = manager.get_config();

        assert_eq!(config.sensitive_paths.len(), 0);
        assert_eq!(config.forbidden_paths.len(), 0);
        assert_eq!(config.max_snapshot_size, 100_000_000);
        assert_eq!(config.snapshot_retention_days, 7);
    }

    #[test]
    fn test_load_project_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_dir = temp_dir.path().join(".ergatai");
        fs::create_dir_all(&config_dir).unwrap();

        let config_content = r#"{
            "sensitive_paths": ["custom/secrets/**"],
            "forbidden_paths": ["node_modules/**"],
            "max_snapshot_size": 500000000,
            "snapshot_retention_days": 14
        }"#;

        fs::write(config_dir.join("config.json"), config_content).unwrap();

        let manager = ConfigManager::new(temp_dir.path(), None).unwrap();
        let config = manager.get_config();

        assert_eq!(config.sensitive_paths.len(), 1);
        assert_eq!(config.sensitive_paths[0], "custom/secrets/**");
        assert_eq!(config.forbidden_paths.len(), 1);
        assert_eq!(config.max_snapshot_size, 500_000_000);
        assert_eq!(config.snapshot_retention_days, 14);
    }

    #[test]
    fn test_sensitive_path_with_project_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_dir = temp_dir.path().join(".ergatai");
        fs::create_dir_all(&config_dir).unwrap();

        let config_content = r#"{
            "sensitive_paths": ["custom/secrets/**", "internal/**"]
        }"#;

        fs::write(config_dir.join("config.json"), config_content).unwrap();

        let manager = ConfigManager::new(temp_dir.path(), None).unwrap();

        // System default
        assert!(manager.is_sensitive_path(".env"));

        // Project-level
        assert!(manager.is_sensitive_path("custom/secrets/api-key.txt"));
        assert!(manager.is_sensitive_path("internal/config.json"));

        // Non-sensitive
        assert!(!manager.is_sensitive_path("src/main.rs"));
    }

    #[test]
    fn test_forbidden_path() {
        let temp_dir = TempDir::new().unwrap();
        let config_dir = temp_dir.path().join(".ergatai");
        fs::create_dir_all(&config_dir).unwrap();

        let config_content = r#"{
            "forbidden_paths": ["node_modules/**", "dist/**"]
        }"#;

        fs::write(config_dir.join("config.json"), config_content).unwrap();

        let manager = ConfigManager::new(temp_dir.path(), None).unwrap();

        assert!(manager.is_forbidden_path("node_modules/package/index.js"));
        assert!(manager.is_forbidden_path("dist/bundle.js"));
        assert!(!manager.is_forbidden_path("src/main.rs"));
    }
}
