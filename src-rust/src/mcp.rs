use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Child;
use tokio::process::Command;

/// MCP server information
#[napi(object)]
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
#[napi(object)]
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
    let mut configs = Vec::new();

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
        Err(_) => return Vec::new(),
    };

    let config: McpConfig = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    config.servers.into_iter().collect()
}

/// Scan all MCP servers from configs
pub fn scan_mcp_servers() -> Result<Vec<McpServerInfo>> {
    let mut servers = Vec::new();
    let mut seen: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    // Scan config files
    for config_path in find_mcp_configs() {
        let is_user_config = config_path
            .parent()
            .map(|p| p.to_string_lossy().contains("ergatai"))
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
pub fn get_mcp_server_config(name: String) -> Result<Option<McpServerInfo>> {
    let servers = scan_mcp_servers()?;
    Ok(servers.into_iter().find(|s| s.name == name))
}

/// Check if a built-in service is available
pub fn check_builtin_services() -> Result<Vec<BuiltinServiceInfo>> {
    let mut services = Vec::new();

    let project_root = std::env::current_dir().unwrap_or(PathBuf::from("."));
    let resource_dir = project_root.join("resource");

    // Check rtk
    let rtk_paths = [
        resource_dir
            .join("rtk")
            .join("target")
            .join("release")
            .join("rtk"),
        PathBuf::from("/usr/local/bin/rtk"),
        PathBuf::from("/usr/bin/rtk"),
    ];
    let rtk_exists = rtk_paths.iter().any(|p| p.exists());
    services.push(BuiltinServiceInfo {
        name: "rtk".to_string(),
        binary_path: rtk_paths[0].to_string_lossy().to_string(),
        status: if rtk_exists {
            "available".to_string()
        } else {
            "missing".to_string()
        },
        version: None,
    });

    // Check codebase-memory-mcp
    let cbm_paths = [
        resource_dir.join("codebase-memory-mcp").join("cbm"),
        PathBuf::from("/usr/local/bin/cbm"),
        PathBuf::from("/usr/bin/cbm"),
    ];
    let cbm_exists = cbm_paths.iter().any(|p| p.exists());
    services.push(BuiltinServiceInfo {
        name: "codebase-memory-mcp".to_string(),
        binary_path: cbm_paths[0].to_string_lossy().to_string(),
        status: if cbm_exists {
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
pub async fn start_mcp_server(name: String) -> Result<String> {
    let config = get_mcp_server_config(name.clone())?.ok_or_else(|| {
        napi::Error::new(
            napi::Status::InvalidArg,
            format!("MCP server not found: {}", name),
        )
    })?;

    let cmd = config.command.ok_or_else(|| {
        napi::Error::new(
            napi::Status::InvalidArg,
            format!("No command configured for {}", name),
        )
    })?;

    // Check if already running
    if is_server_running(&name) {
        return Ok(format!("MCP server '{}' is already running", name));
    }

    let mut command = Command::new(&cmd);
    if let Some(args) = &config.args {
        command.args(args);
    }
    if let Some(env) = &config.env {
        for (k, v) in env {
            command.env(k, v);
        }
    }

    // Spawn the child process
    let child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Failed to spawn MCP server '{}': {}", name, e),
            )
        })?;

    let pid = child.id().unwrap_or(0);

    // Track the process
    if let Ok(mut procs) = mcp_registry().processes.lock() {
        procs.insert(name.clone(), Arc::new(tokio::sync::Mutex::new(child)));
    } else {
        return Err(napi::Error::new(
            napi::Status::GenericFailure,
            "Failed to track MCP server process",
        ));
    }

    tracing::info!(server = %name, pid, "MCP server started");
    Ok(format!("MCP server '{}' started (pid: {})", name, pid))
}

/// Stop a running MCP server.
pub async fn stop_mcp_server(name: String) -> Result<()> {
    let child_arc = {
        let mut procs = mcp_registry().processes.lock().map_err(|_| {
            napi::Error::new(
                napi::Status::GenericFailure,
                "Failed to lock process registry",
            )
        })?;
        procs.remove(&name).ok_or_else(|| {
            napi::Error::new(
                napi::Status::InvalidArg,
                format!("MCP server '{}' is not running", name),
            )
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
