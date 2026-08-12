//! NAPI bindings for file access control
//!
//! Exposes the file access control system to TypeScript via NAPI-RS.
//! Uses a global state pattern (OnceLock<RwLock<FileAccessState>>) to manage
//! FileLockManager, SnapshotManager, and Watchdog instances per project.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use napi_derive::napi;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::acp::sdk_session::{approval_waiters, ApprovalResponse};
use crate::error::ErgataiError;
use crate::file_access::{
    ConfigManager, FileLockManager, FileMode, FileToken, SnapshotManager, SystemToken, Watchdog,
    WatchdogConfig,
};
use crate::napi::to_napi;

/// Per-project file access control state
#[derive(Clone)]
struct ProjectFileAccessState {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
    config_manager: Arc<ConfigManager>,
}

/// Global file access state: project_id → per-project state
struct FileAccessState {
    projects: HashMap<String, ProjectFileAccessState>,
}

static FILE_ACCESS_STATE: OnceLock<RwLock<FileAccessState>> = OnceLock::new();

fn file_access_state() -> &'static RwLock<FileAccessState> {
    FILE_ACCESS_STATE.get_or_init(|| {
        RwLock::new(FileAccessState {
            projects: HashMap::new(),
        })
    })
}

/// Get or initialize file access state for a project
async fn get_project_state(project_id: &str) -> Result<ProjectFileAccessState, ErgataiError> {
    let state = file_access_state();
    let state = state.read().await;

    state.projects.get(project_id).cloned().ok_or_else(|| {
        ErgataiError::NotFound(format!(
            "File access control not initialized for project: {}",
            project_id
        ))
    })
}

/// Initialize file access control system for a project
///
/// Creates lock database, snapshot manager, watchdog, and file system watcher.
/// Returns a handle that can be used to manage the system.
#[napi]
pub async fn file_access_init(project_id: String, project_root: String) -> napi::Result<()> {
    crate::napi::guard();

    let state = file_access_state();
    let mut state = state.write().await;

    // Check if already initialized
    if state.projects.contains_key(&project_id) {
        info!(
            project_id = project_id,
            "File access control already initialized"
        );
        return Ok(());
    }

    let project_root_path = PathBuf::from(&project_root);

    // Create lock database path: {userData}/projects/{project_id}/locks.db
    // For now, use a simple path in the project root (can be enhanced later)
    let lock_db_path = project_root_path.join(".ergatai").join("locks.db");

    // Ensure .ergatai directory exists
    let lock_db_parent = lock_db_path.parent().ok_or_else(|| {
        napi::Error::from_reason(format!(
            "Invalid lock_db_path has no parent: {:?}",
            lock_db_path
        ))
    })?;
    tokio::fs::create_dir_all(lock_db_parent)
        .await
        .map_err(|e| {
            to_napi(ErgataiError::internal(format!(
                "Failed to create .ergatai directory: {}",
                e
            )))
        })?;

    // Try to get NATS client (optional, None = degraded mode)
    let nats_client = if let Some(conn) = crate::nats::get_nats_connection().await {
        info!(project_id = %project_id, "NATS available for file access, enabling multi-agent approval flow");
        Some(Arc::new(conn.client().clone()))
    } else {
        warn!(project_id = %project_id, "NATS not available for file access, running in degraded mode");
        None
    };

    // Create FileLockManager with optional NATS client
    let lock_manager = FileLockManager::new(&lock_db_path, project_root_path.clone(), nats_client)
        .map_err(to_napi)?;

    // If NATS is available, subscribe to approval responses
    if let Err(e) = lock_manager.subscribe_to_nats().await {
        warn!(project_id = %project_id, error = %e, "Failed to subscribe to NATS approval subjects");
    }

    let lock_manager = Arc::new(lock_manager);

    // Create SnapshotManager
    let snapshot_manager = SnapshotManager::new(&project_root_path).map_err(to_napi)?;
    let snapshot_manager = Arc::new(snapshot_manager);

    // Create Watchdog
    let watchdog_config = WatchdogConfig::default();
    let mut watchdog = Watchdog::new(lock_manager.clone(), watchdog_config);
    watchdog.start().map_err(to_napi)?;
    let watchdog = Arc::new(RwLock::new(watchdog));

    // Create ConfigManager (loads .ergatai/config.json if present, with hot reload every 30s)
    let config_manager =
        ConfigManager::new(&project_root_path, Some(std::time::Duration::from_secs(30)))
            .map_err(to_napi)?;
    let config_manager = Arc::new(config_manager);

    // Store in global state
    state.projects.insert(
        project_id.clone(),
        ProjectFileAccessState {
            lock_manager,
            snapshot_manager,
            watchdog,
            config_manager,
        },
    );

    info!(
        project_id = project_id,
        project_root = project_root,
        "File access control system initialized"
    );

    Ok(())
}

/// Register a system token for an agent
#[napi]
pub async fn file_access_register_system_token(
    project_id: String,
    agent_id: String,
    session_id: String,
    project_root: String,
    ttl_secs: u32,
    heartbeat_interval_secs: u32,
) -> napi::Result<String> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let token = SystemToken::new(
        agent_id,
        session_id,
        project_root,
        ttl_secs as u64,
        heartbeat_interval_secs as u64,
    );

    // Register in lock manager
    project_state
        .lock_manager
        .register_system_token(&token)
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = %token.id,
        agent_id = token.agent_id,
        "System token registered"
    );

    Ok(token.id.to_string())
}

/// Request a file access token
#[napi]
pub async fn file_access_request_token(
    project_id: String,
    agent_id: String,
    session_id: String,
    scope: String,
    mode: String,
    reason: Option<String>,
    ttl_secs: u32,
    heartbeat_interval_secs: u32,
    priority: Option<String>,
) -> napi::Result<String> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    // Parse mode
    let file_mode = match mode.to_uppercase().as_str() {
        "READ" => FileMode::Read,
        "WRITE" => FileMode::Write,
        "ADMIN" => FileMode::Admin,
        _ => return Err(napi::Error::from_reason(format!("Invalid mode: {}", mode))),
    };

    // Get system token by session_id
    let system_token = project_state
        .lock_manager
        .find_active_system_token_by_session(&session_id)
        .map_err(to_napi)?;

    // Create FileToken
    let priority_num = crate::file_access::conflict_arbitration::priority_to_number(&priority);
    let file_token = FileToken::with_priority(
        agent_id,
        session_id,
        system_token.id.clone(),
        scope,
        file_mode,
        reason,
        "system".to_string(), // TODO: Implement approval flow
        ttl_secs as u64,
        heartbeat_interval_secs as u64,
        priority_num,
    );

    // Register FileToken
    project_state
        .lock_manager
        .register_file_token(&file_token)
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        agent_id = file_token.agent_id,
        session_id = file_token.session_id,
        scope = file_token.scope,
        mode = ?file_token.mode,
        token_id = %file_token.id,
        "File access token requested and registered"
    );

    Ok(file_token.id.to_string())
}

/// Acquire a file lock
#[napi]
pub async fn file_access_acquire_lock(
    project_id: String,
    token_id: String,
    file_path: String,
) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    // Get the actual FileToken by token_id
    let file_token = project_state
        .lock_manager
        .find_active_file_token_by_id(&token_id)
        .map_err(to_napi)?;

    // Check scope
    if !file_token.matches_path(&file_path) {
        return Err(napi::Error::from_reason(format!(
            "File {} is outside scope {}",
            file_path, file_token.scope
        )));
    }

    // Acquire file lock
    project_state
        .lock_manager
        .acquire_lock(&file_token, &file_path)
        .await
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = token_id,
        file_path = file_path,
        "File lock acquired"
    );

    Ok(())
}

/// Release a file lock
#[napi]
pub async fn file_access_release_lock(
    project_id: String,
    token_id: String,
    file_path: String,
) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    project_state
        .lock_manager
        .release_lock(&token_id, &file_path)
        .await
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = token_id,
        file_path = file_path,
        "File lock released"
    );

    Ok(())
}

/// Upgrade an existing READ lock to WRITE (deadlock-safe).
///
/// Follows the release-READ → acquire-WRITE pattern so the upgrade goes through
/// proper conflict arbitration, single-agent bypass, and retry tracking.
/// If the WRITE acquisition fails, the READ lock is restored.
#[napi]
pub async fn file_access_upgrade_lock(
    project_id: String,
    token_id: String,
    file_path: String,
) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    // Get the current READ FileToken (so we can construct a WRITE token for acquire_lock)
    let read_token = project_state
        .lock_manager
        .find_active_file_token_by_id(&token_id)
        .map_err(to_napi)?;

    // Delegate to FileLockManager.upgrade_to_write which does:
    // 1. Verify READ lock exists
    // 2. Release READ
    // 3. Acquire WRITE (through full arbitration path)
    // 4. On failure → restore READ
    project_state
        .lock_manager
        .upgrade_to_write(&read_token, &file_path)
        .await
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = token_id,
        file_path = file_path,
        "Lock upgraded from READ to WRITE"
    );

    Ok(())
}

/// Downgrade an existing WRITE lock to READ.
#[napi]
pub async fn file_access_downgrade_lock(
    project_id: String,
    token_id: String,
    file_path: String,
) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let write_token = project_state
        .lock_manager
        .find_active_file_token_by_id(&token_id)
        .map_err(to_napi)?;

    project_state
        .lock_manager
        .downgrade_to_read(&write_token, &file_path)
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = token_id,
        file_path = file_path,
        "Lock downgraded from WRITE to READ"
    );

    Ok(())
}

/// Read the latest version of a file (READ_LATEST semantics)
///
/// Waits for any pending WRITE to complete before reading.
#[napi]
pub async fn file_access_read_latest(
    project_id: String,
    file_path: String,
) -> napi::Result<Vec<u8>> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let content = project_state
        .lock_manager
        .read_latest(&file_path)
        .await
        .map_err(to_napi)?;

    Ok(content)
}

/// Create a snapshot of a file before modification
#[napi]
pub async fn file_access_create_snapshot(
    project_id: String,
    file_path: String,
    agent_id: String,
) -> napi::Result<String> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    // M3 fix: Wrap blocking snapshot creation in spawn_blocking to avoid
    // blocking the tokio runtime (fs::read + git blob creation are I/O-heavy)
    let snapshot_manager = project_state.snapshot_manager.clone();
    let fp = file_path.clone();
    let aid = agent_id.clone();
    let git_hash = tokio::task::spawn_blocking(move || snapshot_manager.create_snapshot(&fp, &aid))
        .await
        .map_err(|e| napi::Error::from_reason(format!("spawn_blocking join error: {}", e)))?
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        file_path = file_path,
        agent_id = agent_id,
        git_hash = git_hash,
        "File snapshot created"
    );

    Ok(git_hash)
}

/// Mark a session as busy (task-aware heartbeat)
#[napi]
pub async fn file_access_mark_busy(
    project_id: String,
    session_id: String,
    duration_secs: u32,
) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let watchdog = project_state.watchdog.read().await;
    watchdog
        .mark_busy(&session_id, duration_secs as u64)
        .await
        .map_err(to_napi)?;

    info!(
        project_id = project_id,
        session_id = session_id,
        duration_secs = duration_secs,
        "Session marked as busy"
    );

    Ok(())
}

/// Clear busy status for a session
#[napi]
pub async fn file_access_clear_busy(project_id: String, session_id: String) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let watchdog = project_state.watchdog.read().await;
    watchdog.clear_busy(&session_id).await.map_err(to_napi)?;

    info!(
        project_id = project_id,
        session_id = session_id,
        "Session busy status cleared"
    );

    Ok(())
}

/// Shutdown file access control system for a project
#[napi]
pub async fn file_access_shutdown(project_id: String) -> napi::Result<()> {
    crate::napi::guard();

    let state = file_access_state();
    let mut state = state.write().await;

    if let Some(project_state) = state.projects.remove(&project_id) {
        // Stop NATS subscription first (prevents Arc<FileLockManager> leak)
        project_state.lock_manager.shutdown_nats_subscription();

        // Stop watchdog
        let mut watchdog = project_state.watchdog.write().await;
        watchdog.stop().map_err(to_napi)?;

        info!(
            project_id = project_id,
            "File access control system shutdown"
        );
    } else {
        warn!(
            project_id = project_id,
            "File access control not initialized for project"
        );
    }

    Ok(())
}

/// Respond to an approval request from TypeScript
///
/// Called when the main agent (or auto-approval logic) has made a decision
/// about a file access permission request.
#[napi]
pub async fn file_access_respond_approval(
    request_id: String,
    approved: bool,
    approved_by: String,
    reason: Option<String>,
) -> napi::Result<()> {
    crate::napi::guard();

    // Get the approval waiters lock
    let mut waiters = approval_waiters()
        .lock()
        .map_err(|_| napi::Error::from_reason("Failed to acquire approval waiters lock"))?;

    // Remove the waiter and send the response
    if let Some(tx) = waiters.remove(&request_id) {
        let response = ApprovalResponse {
            approved,
            approved_by,
            reason,
        };

        // Send the response; if the receiver is gone, this will fail silently
        let _ = tx.send(response);

        info!(
            request_id = request_id,
            approved = approved,
            "Approval response sent"
        );

        Ok(())
    } else {
        warn!(
            request_id = request_id,
            "Approval request not found (may have timed out)"
        );
        Ok(())
    }
}

/// Check if a file path is sensitive (requires ADMIN permission)
///
/// Checks both system defaults and project-level configuration.
#[napi]
pub async fn file_access_is_sensitive_path(
    project_id: String,
    file_path: String,
) -> napi::Result<bool> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let is_sensitive = project_state.config_manager.is_sensitive_path(&file_path);

    Ok(is_sensitive)
}

/// Check if a file path is forbidden (completely blocked)
///
/// Uses project-level configuration only.
#[napi]
pub async fn file_access_is_forbidden_path(
    project_id: String,
    file_path: String,
) -> napi::Result<bool> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    let is_forbidden = project_state.config_manager.is_forbidden_path(&file_path);

    Ok(is_forbidden)
}

/// Manually reload the project configuration
#[napi]
pub async fn file_access_reload_config(project_id: String) -> napi::Result<()> {
    crate::napi::guard();

    let project_state = get_project_state(&project_id).await.map_err(to_napi)?;

    project_state.config_manager.reload().map_err(to_napi)?;

    info!(project_id = project_id, "Configuration reloaded");

    Ok(())
}
