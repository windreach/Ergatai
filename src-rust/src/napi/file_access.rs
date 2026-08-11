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

use crate::error::ErgataiError;
use crate::file_access::{
    FileLockManager, FileMode, FileToken, SnapshotManager, SystemToken, Watchdog, WatchdogConfig,
};
use crate::napi::to_napi;
use crate::acp::sdk_session::{ApprovalResponse, approval_waiters};

/// Per-project file access control state
#[derive(Clone)]
struct ProjectFileAccessState {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
}

/// Global file access state: project_id → per-project state
struct FileAccessState {
    projects: HashMap<String, ProjectFileAccessState>,
}

static FILE_ACCESS_STATE: OnceLock<RwLock<FileAccessState>> = OnceLock::new();

fn file_access_state() -> &'static RwLock<FileAccessState> {
    FILE_ACCESS_STATE.get_or_init(|| RwLock::new(FileAccessState {
        projects: HashMap::new(),
    }))
}

/// Get or initialize file access state for a project
async fn get_project_state(project_id: &str) -> Result<ProjectFileAccessState, ErgataiError> {
    let state = file_access_state();
    let state = state.read().await;

    state
        .projects
        .get(project_id)
        .cloned()
        .ok_or_else(|| {
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
pub async fn file_access_init(
    project_id: String,
    project_root: String,
) -> napi::Result<()> {
    crate::napi::guard();

    let state = file_access_state();
    let mut state = state.write().await;

    // Check if already initialized
    if state.projects.contains_key(&project_id) {
        info!(project_id = project_id, "File access control already initialized");
        return Ok(());
    }

    let project_root_path = PathBuf::from(&project_root);

    // Create lock database path: {userData}/projects/{project_id}/locks.db
    // For now, use a simple path in the project root (can be enhanced later)
    let lock_db_path = project_root_path.join(".ergatai").join("locks.db");

    // Ensure .ergatai directory exists
    let lock_db_parent = lock_db_path.parent().ok_or_else(|| {
        napi::Error::from_reason(format!(
            "Invalid lock_db_path has no parent: {:?}", lock_db_path
        ))
    })?;
    tokio::fs::create_dir_all(lock_db_parent)
        .await
        .map_err(|e| to_napi(ErgataiError::internal(format!("Failed to create .ergatai directory: {}", e))))?;

    // Create FileLockManager
    let lock_manager = FileLockManager::new(&lock_db_path, project_root_path.clone())
        .map_err(to_napi)?;
    let lock_manager = Arc::new(lock_manager);

    // Create SnapshotManager
    let snapshot_manager = SnapshotManager::new(&project_root_path)
        .map_err(to_napi)?;
    let snapshot_manager = Arc::new(snapshot_manager);

    // Create Watchdog
    let watchdog_config = WatchdogConfig::default();
    let mut watchdog = Watchdog::new(lock_manager.clone(), watchdog_config);
    watchdog.start().map_err(to_napi)?;
    let watchdog = Arc::new(RwLock::new(watchdog));

    // Store in global state
    state.projects.insert(project_id.clone(), ProjectFileAccessState {
        lock_manager,
        snapshot_manager,
        watchdog,
    });

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
    project_state.lock_manager.register_system_token(&token).map_err(to_napi)?;

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
    let file_token = FileToken::new(
        agent_id,
        session_id,
        system_token.id.clone(),
        scope,
        file_mode,
        reason,
        "system".to_string(), // TODO: Implement approval flow
        ttl_secs as u64,
        heartbeat_interval_secs as u64,
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

    project_state.lock_manager.release_lock(&token_id, &file_path).await.map_err(to_napi)?;

    info!(
        project_id = project_id,
        token_id = token_id,
        file_path = file_path,
        "File lock released"
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

    let content = project_state.lock_manager.read_latest(&file_path).await.map_err(to_napi)?;

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

    let git_hash = project_state.snapshot_manager.create_snapshot(&file_path, &agent_id).map_err(to_napi)?;

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
    watchdog.mark_busy(&session_id, duration_secs as u64).await.map_err(to_napi)?;

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
pub async fn file_access_clear_busy(
    project_id: String,
    session_id: String,
) -> napi::Result<()> {
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
        // Stop watchdog
        let mut watchdog = project_state.watchdog.write().await;
        watchdog.stop().map_err(to_napi)?;

        info!(project_id = project_id, "File access control system shutdown");
    } else {
        warn!(project_id = project_id, "File access control not initialized for project");
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
    let mut waiters = approval_waiters().lock().map_err(|_| {
        napi::Error::from_reason("Failed to acquire approval waiters lock")
    })?;

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
