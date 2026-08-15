//! MCP server configuration scanning and lifecycle management.
//!
//! `scan_mcp_servers` is called from `acp::sdk_session` to inject user-configured
//! MCP servers into every agent session. The `start_mcp_server` / `stop_mcp_server`
//! / `check_builtin_services` / `get_mcp_server_config` entry points are reserved
//! for a future NAPI/tauri frontend surface that lets users manage MCP server
//! lifecycles from the UI; they are intentionally kept but not yet wired up.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Child;
use tokio::process::Command;

use ergatai_error::{ErgataiError, ErgataiResult};

/// MCP server information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub id: String,
    pub name: String,
    pub category: String, // "user" | "built-in"
    pub status: String,   // "running" | "stopped" | "error"
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<std::collections::HashMap<String, String>>,
}

/// Built-in service info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinServiceInfo {
    pub name: String,
    pub binary_path: String,
    pub status: String, // "available" | "missing" | "building"
    pub version: Option<String>,
}

/// MCP server entry from config
#[derive(Deserialize, Debug, Clone)]
struct McpServerEntry {
    #[serde(rename = "command")]
    cmd: Option<String>,
    #[serde(rename = "args", default)]
    args: Option<Vec<String>>,
    #[serde(rename = "env", default)]
    env: Option<std::collections::HashMap<String, String>>,
}

/// MCP config file structure
#[derive(Deserialize, Debug)]
struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    servers: std::collections::HashMap<String, McpServerEntry>,
}

/// Find MCP config files
fn find_mcp_configs() -> Vec<PathBuf> {
    // At most 3 configs: user config, local .mcp.json, local mcp.json
    let mut configs = Vec::with_capacity(3);

    // 1. User config
    if let Some(config_dir) = dirs::config_dir() {
        let user_config = config_dir.join("ergatai").join("mcp.json");
        if user_config.exists() {
            configs.push(user_config);
        }
    }

    // 2. Local project config
    if let Ok(cwd) = std::env::current_dir() {
        let local_config = cwd.join(".mcp.json");
        if local_config.exists() {
            configs.push(local_config);
        }
        let local_config2 = cwd.join("mcp.json");
        if local_config2.exists() {
            configs.push(local_config2);
        }
    }

    configs
}

/// Parse an MCP config file and return server entries
fn parse_mcp_config(path: &Path) -> Vec<(String, McpServerEntry)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Failed to read MCP config '{}': {}",
                path.display(),
                e
            );
            return Vec::new();
        }
    };

    let config: McpConfig = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                "Failed to parse MCP config '{}' — check JSON syntax: {}",
                path.display(),
                e
            );
            return Vec::new();
        }
    };

    config.servers.into_iter().collect()
}

/// Scan all MCP servers from configs
pub fn scan_mcp_servers() -> ErgataiResult<Vec<McpServerInfo>> {
    // Pre-allocate with a reasonable default capacity
    let mut servers = Vec::with_capacity(8);
    let mut seen: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    // Determine the canonical user config directory once
    let user_config_dir: Option<PathBuf> = dirs::config_dir()
        .map(|d| d.join("ergatai"));

    // Scan config files
    for config_path in find_mcp_configs() {
        // Robust category detection: compare against canonical user config directory
        // instead of substring matching on the path string
        let is_user_config = user_config_dir
            .as_ref()
            .and_then(|user_dir| config_path.parent().map(|p| p == user_dir))
            .unwrap_or(false);

        for (name, entry) in parse_mcp_config(&config_path) {
            if seen.contains_key(&name) {
                continue;
            }
            seen.insert(name.clone(), true);

            let status = if is_server_running(&name) {
                "running"
            } else {
                "stopped"
            };

            servers.push(McpServerInfo {
                id: format!("mcp-{}", name),
                name: name.clone(),
                category: if is_user_config {
                    "user".to_string()
                } else {
                    "workspace".to_string()
                },
                status: status.to_string(),
                command: entry.cmd,
                args: entry.args,
                env: entry.env,
            });
        }
    }

    // Add built-in MCP servers (from resource/)
    let project_root = std::env::current_dir().unwrap_or(PathBuf::from("."));
    let resource_dir = project_root.join("resource");

    // Check for codebase-memory-mcp
    let cbm_binary = resource_dir.join("codebase-memory-mcp").join("cbm");
    if cbm_binary.exists() {
        if !seen.contains_key("codebase-memory") {
            let status = if is_server_running("codebase-memory") {
                "running"
            } else {
                "stopped"
            };
            servers.push(McpServerInfo {
                id: "mcp-builtin-codebase-memory".to_string(),
                name: "codebase-memory".to_string(),
                category: "built-in".to_string(),
                status: status.to_string(),
                command: Some(cbm_binary.to_string_lossy().to_string()),
                args: Some(vec![
                    "server".to_string(),
                    "--project".to_string(),
                    ".".to_string(),
                ]),
                env: None,
            });
        }
        seen.insert("codebase-memory".to_string(), true);
    }

    // Sort by category then name
    servers.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(servers)
}

/// Get a specific MCP server's configuration
pub fn get_mcp_server_config(name: String) -> ErgataiResult<Option<McpServerInfo>> {
    let servers = scan_mcp_servers()?;
    Ok(servers.into_iter().find(|s| s.name == name))
}

/// Check if a built-in service is available
pub fn check_builtin_services() -> ErgataiResult<Vec<BuiltinServiceInfo>> {
    let mut services = Vec::new();

    let project_root = std::env::current_dir().unwrap_or(PathBuf::from("."));
    let resource_dir = project_root.join("resource");

    // Check rtk — report the first path that actually exists
    let rtk_paths = [
        resource_dir
            .join("rtk")
            .join("target")
            .join("release")
            .join("rtk"),
        PathBuf::from("/usr/local/bin/rtk"),
        PathBuf::from("/usr/bin/rtk"),
    ];
    let rtk_found = rtk_paths.iter().find(|p| p.exists());
    services.push(BuiltinServiceInfo {
        name: "rtk".to_string(),
        binary_path: rtk_found
            .unwrap_or(&rtk_paths[0])
            .to_string_lossy()
            .to_string(),
        status: if rtk_found.is_some() {
            "available".to_string()
        } else {
            "missing".to_string()
        },
        version: None,
    });

    // Check codebase-memory-mcp — report the first path that actually exists
    let cbm_paths = [
        resource_dir.join("codebase-memory-mcp").join("cbm"),
        PathBuf::from("/usr/local/bin/cbm"),
        PathBuf::from("/usr/bin/cbm"),
    ];
    let cbm_found = cbm_paths.iter().find(|p| p.exists());
    services.push(BuiltinServiceInfo {
        name: "codebase-memory-mcp".to_string(),
        binary_path: cbm_found
            .unwrap_or(&cbm_paths[0])
            .to_string_lossy()
            .to_string(),
        status: if cbm_found.is_some() {
            "available".to_string()
        } else {
            "missing".to_string()
        },
        version: None,
    });

    Ok(services)
}

// ── Running MCP server process management ──

/// Global state for tracking running MCP server processes.
struct McpProcessRegistry {
    processes: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Child>>>>,
}

static MCP_REGISTRY: std::sync::OnceLock<McpProcessRegistry> = std::sync::OnceLock::new();

fn mcp_registry() -> &'static McpProcessRegistry {
    MCP_REGISTRY.get_or_init(|| McpProcessRegistry {
        processes: std::sync::Mutex::new(HashMap::new()),
    })
}

/// Check if a server is currently running.
///
/// Probes the child process with `try_wait()` so a crashed or exited server is
/// detected and pruned from the registry instead of being reported as
/// "running" forever. If the inner tokio Mutex is currently held (e.g. by
/// `stop_mcp_server`), we optimistically report the server as running — the
/// caller that holds the lock will clean up.
fn is_server_running(name: &str) -> bool {
    let mut procs = match mcp_registry().processes.lock() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let child_arc = match procs.get(name) {
        Some(arc) => Arc::clone(arc),
        None => return false,
    };

    // Try to probe the child without blocking. If another caller holds the
    // tokio lock we can't check right now — treat it as still running.
    let mut child_guard = match child_arc.try_lock() {
        Ok(g) => g,
        Err(_) => return true,
    };

    match child_guard.try_wait() {
        Ok(Some(status)) => {
            // Process has exited — drop our guard, then prune the registry.
            drop(child_guard);
            procs.remove(name);
            tracing::info!(
                server = %name,
                status = ?status,
                "MCP server process exited, removed from registry"
            );
            false
        }
        Ok(None) => true, // Still running
        Err(e) => {
            // wait error — treat as dead and prune
            drop(child_guard);
            procs.remove(name);
            tracing::warn!(
                server = %name,
                error = %e,
                "MCP server wait failed, removed from registry"
            );
            false
        }
    }
}

/// Start an MCP server as a child process.
///
/// Spawns the configured command and tracks the child process so it can be
/// stopped later via [`stop_mcp_server`].
///
/// The check-then-act sequence (is_already_running → spawn → insert) is performed
/// while holding the registry lock to prevent concurrent duplicate starts.
pub async fn start_mcp_server(name: String) -> ErgataiResult<String> {
    let config = get_mcp_server_config(name.clone())?
        .ok_or_else(|| ErgataiError::internal(format!("MCP server not found: {}", name)))?;

    let cmd = config
        .command
        .ok_or_else(|| ErgataiError::internal(format!("No command configured for {}", name)))?;

    let mut command = Command::new(&cmd);
    if let Some(args) = &config.args {
        command.args(args);
    }
    if let Some(env) = &config.env {
        for (k, v) in env {
            command.env(k, v);
        }
    }

    // Hold the registry lock across the check + spawn + insert sequence
    // to prevent concurrent duplicate starts (check-then-act race).
    let mut procs = mcp_registry()
        .processes
        .lock()
        .map_err(|_| ErgataiError::internal("Failed to acquire MCP process registry lock".to_string()))?;

    // Check if already running (under the lock)
    if let Some(child_arc) = procs.get(&name) {
        // Probe to see if the process is still alive
        let mut child_guard = match child_arc.try_lock() {
            Ok(g) => g,
            Err(_) => {
                // Another caller holds the tokio lock — treat as running
                return Ok(format!("MCP server '{}' is already running", name));
            }
        };
        match child_guard.try_wait() {
            Ok(Some(_status)) => {
                // Process exited — prune stale entry, fall through to respawn
                drop(child_guard);
                procs.remove(&name);
            }
            Ok(None) => {
                // Still running
                return Ok(format!("MCP server '{}' is already running", name));
            }
            Err(_) => {
                // wait error — treat as dead, prune and respawn
                drop(child_guard);
                procs.remove(&name);
            }
        }
    }

    // Spawn the child process (still under registry lock)
    let child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            ErgataiError::internal(format!("Failed to spawn MCP server '{}': {}", name, e))
        })?;

    let pid = child.id().unwrap_or(0);

    // Insert into registry (under the same lock acquisition)
    procs.insert(name.clone(), Arc::new(tokio::sync::Mutex::new(child)));

    tracing::info!(server = %name, pid, "MCP server started");
    Ok(format!("MCP server '{}' started (pid: {})", name, pid))
}

/// Stop a running MCP server.
pub async fn stop_mcp_server(name: String) -> ErgataiResult<()> {
    let child_arc = {
        let mut procs = mcp_registry()
            .processes
            .lock()
            .map_err(|_| ErgataiError::internal("Failed to lock process registry".to_string()))?;
        procs.remove(&name).ok_or_else(|| {
            ErgataiError::internal(format!("MCP server '{}' is not running", name))
        })?
    };

    // Drop the Arc — if this is the last reference, the Child is dropped and
    // kill_on_drop(true) ensures the process is killed.
    let mut child = child_arc.lock().await;
    if let Err(e) = child.kill().await {
        tracing::warn!(server = %name, error = %e, "Failed to kill MCP server process");
    } else {
        tracing::info!(server = %name, "MCP server stopped");
    }

    Ok(())
}

/// Stop all running MCP servers.
///
/// Best-effort: logs but does not abort on individual failures. Called during
/// graceful shutdown to prevent MCP child processes from leaking.
pub async fn stop_all_mcp_servers() {
    // Drain the registry first so no new servers can be added mid-shutdown.
    let entries: Vec<(String, Arc<tokio::sync::Mutex<Child>>)> = {
        let mut procs = match mcp_registry().processes.lock() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Failed to lock MCP process registry during shutdown");
                return;
            }
        };
        procs.drain().collect()
    };

    if entries.is_empty() {
        return;
    }

    tracing::info!(count = entries.len(), "Stopping all MCP servers...");
    for (name, child_arc) in entries {
        let mut child = child_arc.lock().await;
        if let Err(e) = child.kill().await {
            tracing::warn!(server = %name, error = %e, "Failed to kill MCP server process");
        } else {
            tracing::info!(server = %name, "MCP server stopped");
        }
    }
}
