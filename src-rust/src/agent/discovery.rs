//! Discovery logic for ACP runtimes.
//!
//! Detects installed agents, probes their authentication status, and builds
//! a catalog of available runtimes for the frontend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use napi_derive::napi;
use serde::{Deserialize, Serialize};

use super::runtime_metadata::KNOWN_ACP_RUNTIMES;

/// Availability status of an ACP runtime.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[napi(string_enum)]
pub enum AcpAvailabilityStatus {
    /// Binary found and ready to use.
    Available,
    /// Binary not found on the system.
    NotInstalled,
    /// Binary found but authentication required.
    AuthRequired,
}

/// Authentication status of an ACP runtime.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[napi(string_enum)]
pub enum AuthStatus {
    /// User is authenticated.
    LoggedIn,
    /// User is not authenticated.
    LoggedOut,
    /// Authentication check not applicable for this runtime.
    NotApplicable,
    /// Authentication status unknown (probe failed or not run).
    Unknown,
}

/// Frontend-facing catalog entry for an ACP runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[napi(object)]
pub struct AcpRuntimeCatalogEntry {
    pub id: String,
    pub label: String,
    pub avatar_url: String,
    pub availability: AcpAvailabilityStatus,
    pub command: Option<String>,
    pub binary_path: Option<String>,
    pub install_hint: String,
    pub install_instructions_url: String,
    pub has_install_command: bool,  // Whether this runtime has a predefined install command
    pub auth_status: AuthStatus,
    pub login_hint: Option<String>,
    pub source: String, // "builtin" | "custom"
}

/// Cache for resolved command paths (per-app-lifetime).
fn resolve_cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Common binary paths to search (in addition to PATH).
fn common_binary_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];

    if let Some(home) = dirs::home_dir() {
        paths.extend([
            home.join(".local/bin"),
            home.join(".volta/bin"),
            home.join(".bun/bin"),
            home.join(".asdf/shims"),
        ]);
    }

    paths
}

/// Resolve a command to an absolute path.
///
/// Resolution order:
/// 1. Cache check
/// 2. PATH environment variable
/// 3. Common binary paths (homebrew, local bin, etc.)
/// 4. Login shell (zsh -l -c 'command -v ...')
pub fn resolve_command(command: &str) -> Option<PathBuf> {
    let cache = resolve_cache();

    // Fast path: return cached result
    if let Ok(guard) = cache.lock() {
        if let Some(result) = guard.get(command) {
            return result.clone();
        }
    }

    // Slow path: resolve and cache
    let result = resolve_command_uncached(command);

    if let Ok(mut guard) = cache.lock() {
        guard.insert(command.to_string(), result.clone());
    }

    result
}

/// Resolve a command without using the cache.
fn resolve_command_uncached(command: &str) -> Option<PathBuf> {
    // If command looks like a path, check it directly
    if command.contains('/') || command.contains('\\') {
        let path = PathBuf::from(command);
        return if path.is_file() { Some(path) } else { None };
    }

    // Check PATH environment variable
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }

            // On Windows, also check .exe/.cmd/.bat
            #[cfg(windows)]
            {
                for ext in &["exe", "cmd", "bat"] {
                    let candidate = dir.join(format!("{}.{}", command, ext));
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // Check common binary paths
    for dir in common_binary_paths() {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }

        #[cfg(windows)]
        {
            for ext in &["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{}.{}", command, ext));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    // Try login shell (Unix only)
    #[cfg(unix)]
    {
        // Validate command name to prevent shell injection
        if !command.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return None;
        }

        if let Ok(output) = Command::new("zsh")
            .args(["-l", "-c", &format!("command -v {}", command)])
            .output()
        {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path_str.is_empty() {
                    let path = PathBuf::from(path_str);
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

/// Probe authentication status for a runtime.
fn probe_auth_status(binary: &PathBuf, args: &[&str]) -> AuthStatus {
    let result = Command::new(binary)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) => {
            if output.status.success() {
                AuthStatus::LoggedIn
            } else {
                // Check stderr for specific error patterns
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("not logged in") || stderr.contains("not authenticated") {
                    AuthStatus::LoggedOut
                } else {
                    AuthStatus::Unknown
                }
            }
        }
        Err(_) => AuthStatus::Unknown,
    }
}

/// Discover all known ACP runtimes and their availability.
pub fn discover_acp_runtimes() -> Vec<AcpRuntimeCatalogEntry> {
    let mut entries = Vec::new();

    // Phase 1: Binary resolution (fast, no probes)
    struct PartialEntry {
        entry: AcpRuntimeCatalogEntry,
        auth_probe_args: Option<Vec<String>>,
        binary_path: Option<PathBuf>,
    }

    let mut partials: Vec<PartialEntry> = KNOWN_ACP_RUNTIMES
        .iter()
        .map(|runtime| {
            // Try to find the binary
            let mut binary_path = None;
            let mut resolved_command = None;

            for cmd in runtime.commands {
                if let Some(path) = resolve_command(cmd) {
                    binary_path = Some(path.clone());
                    resolved_command = Some(cmd.to_string());
                    break;
                }
            }

            // Also check underlying_cli if no ACP adapter found
            if binary_path.is_none() {
                if let Some(underlying) = runtime.underlying_cli {
                    if let Some(path) = resolve_command(underlying) {
                        binary_path = Some(path);
                    }
                }
            }

            let availability = if binary_path.is_some() {
                AcpAvailabilityStatus::Available
            } else {
                AcpAvailabilityStatus::NotInstalled
            };

            // Construct avatar URL: use local file as base64 data URL if available
            let avatar_url = if !runtime.avatar_file.is_empty() {
                let icon_path = if let Some(resources_path) = crate::get_resources_path() {
                    let path = resources_path.join("agent-icons").join(runtime.avatar_file);
                    if path.exists() {
                        Some(path)
                    } else {
                        tracing::debug!(path = %path.display(), "Icon not found in resources path, trying dev path");
                        // Fallback: try development path (project root/resources)
                        std::env::current_dir()
                            .ok()
                            .map(|d| d.join("resources").join("agent-icons").join(runtime.avatar_file))
                            .filter(|p| p.exists())
                    }
                } else {
                    tracing::debug!(agent = %runtime.id, "Resources path not set, trying dev path");
                    // No resources path set, try dev path directly
                    std::env::current_dir()
                        .ok()
                        .map(|d| d.join("resources").join("agent-icons").join(runtime.avatar_file))
                        .filter(|p| p.exists())
                };

                if let Some(path) = icon_path {
                    tracing::debug!(path = %path.display(), "Loading agent icon");
                    // Read file and convert to base64 data URL
                    match std::fs::read(&path) {
                        Ok(data) => {
                            use base64::{Engine as _, engine::general_purpose};
                            let base64_data = general_purpose::STANDARD.encode(&data);
                            let mime_type = if path.extension().and_then(|e| e.to_str()) == Some("png") {
                                "image/png"
                            } else {
                                "image/jpeg"
                            };
                            let url = format!("data:{};base64,{}", mime_type, base64_data);
                            tracing::debug!(agent = %runtime.id, url_len = url.len(), "Icon loaded successfully");
                            url
                        }
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "Failed to read agent icon");
                            String::new()
                        }
                    }
                } else {
                    tracing::debug!(agent = %runtime.id, file = %runtime.avatar_file, "Icon file not found");
                    String::new()
                }
            } else {
                String::new()
            };

            let entry = AcpRuntimeCatalogEntry {
                id: runtime.id.to_string(),
                label: runtime.label.to_string(),
                avatar_url,
                availability,
                command: resolved_command,
                binary_path: binary_path.as_ref().map(|p| p.display().to_string()),
                install_hint: runtime.install_hint.to_string(),
                install_instructions_url: runtime.install_instructions_url.to_string(),
                has_install_command: runtime.install_command.is_some(),
                auth_status: AuthStatus::Unknown, // Will be updated in Phase 2
                login_hint: runtime.login_hint.map(|s| s.to_string()),
                source: "builtin".to_string(),
            };

            let auth_probe_args = runtime.auth_probe_args.map(|args| {
                args.iter().map(|s| s.to_string()).collect()
            });

            PartialEntry {
                entry,
                auth_probe_args,
                binary_path,
            }
        })
        .collect();

    // Phase 2: Auth probes (parallel)
    // For simplicity, we'll do this sequentially for now
    for partial in &mut partials {
        if partial.entry.availability != AcpAvailabilityStatus::Available {
            continue;
        }

        if let (Some(args), Some(binary)) = (&partial.auth_probe_args, &partial.binary_path) {
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let status = probe_auth_status(binary, &args_refs);
            partial.entry.auth_status = status;

            // Update availability based on auth status
            if status == AuthStatus::LoggedOut {
                partial.entry.availability = AcpAvailabilityStatus::AuthRequired;
            }
        } else {
            // No auth probe needed
            partial.entry.auth_status = AuthStatus::NotApplicable;
        }
    }

    // Collect results
    for partial in partials {
        entries.push(partial.entry);
    }

    // Phase 3: Load custom harnesses from <app-data>/custom_harnesses/*.json
    if let Ok(custom_harnesses) = super::custom_harness::load_custom_harnesses() {
        for harness in custom_harnesses {
            // Check if command is available
            let binary_path = resolve_command(&harness.command);
            let availability = if binary_path.is_some() {
                AcpAvailabilityStatus::Available
            } else {
                AcpAvailabilityStatus::NotInstalled
            };

            let entry = AcpRuntimeCatalogEntry {
                id: harness.id,
                label: harness.label,
                avatar_url: String::new(), // Custom harnesses don't have avatars
                availability,
                command: Some(harness.command),
                binary_path: binary_path.map(|p| p.display().to_string()),
                install_hint: harness.install_hint,
                install_instructions_url: harness.install_instructions_url,
                has_install_command: false,  // Custom harnesses don't have predefined install commands
                auth_status: AuthStatus::NotApplicable, // Custom harnesses don't have auth probes
                login_hint: None,
                source: "custom".to_string(),
            };

            entries.push(entry);
        }
    }

    entries
}

/// Clear the resolve_command cache (e.g., after installing a new agent).
pub fn clear_resolve_cache() {
    if let Ok(mut guard) = resolve_cache().lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::runtime_metadata::normalize_command_identity;

    #[test]
    fn test_normalize_command_identity() {
        assert_eq!(normalize_command_identity("goose"), "goose");
        assert_eq!(normalize_command_identity("Claude_Code"), "claude-code");
    }

    #[test]
    fn test_discover_acp_runtimes() {
        let runtimes = discover_acp_runtimes();
        assert!(!runtimes.is_empty());

        // Check that all known runtimes are present
        let ids: Vec<&str> = runtimes.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"goose"));
        assert!(ids.contains(&"claude"));
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"hermes"));
    }
}
