//! Global file access manager for application-wide file access control
//!
//! Provides lazy initialization of FileLockManager, SnapshotManager, and Watchdog.
//! Similar to NatsManager, this provides a central point for file access control.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::error::ErgataiError;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::ErgataiResult;
use crate::file_access::{FileLockManager, SnapshotManager, Watchdog, WatchdogConfig};

/// Per-project file access control state
struct ProjectFileAccess {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
}

/// Global file access state
struct FileAccessManagerState {
    projects: HashMap<String, ProjectFileAccess>,
}

static FILE_ACCESS_MANAGER: OnceLock<RwLock<FileAccessManagerState>> = OnceLock::new();

fn file_access_manager() -> &'static RwLock<FileAccessManagerState> {
    FILE_ACCESS_MANAGER.get_or_init(|| RwLock::new(FileAccessManagerState {
        projects: HashMap::new(),
    }))
}

/// Initialize file access control for a project
///
/// Creates lock database, snapshot manager, and watchdog.
/// Idempotent - calling multiple times is safe.
pub async fn init_file_access(project_id: &str, project_root: &PathBuf) -> ErgataiResult<()> {
    let manager = file_access_manager();
    let mut manager = manager.write().await;

    // Check if already initialized
    if manager.projects.contains_key(project_id) {
        info!(project_id = project_id, "File access control already initialized");
        return Ok(());
    }

    // Create lock database path
    let lock_db_path = project_root.join(".ergatai").join("locks.db");

    // Ensure .ergatai directory exists
    let lock_db_parent = lock_db_path.parent().ok_or_else(|| {
        ErgataiError::InvalidArgument(format!(
            "Invalid lock_db_path has no parent: {:?}", lock_db_path
        ))
    })?;
    tokio::fs::create_dir_all(lock_db_parent).await?;

    // Create FileLockManager
    let lock_manager = FileLockManager::new(&lock_db_path, project_root.clone())?;
    let lock_manager = Arc::new(lock_manager);

    // Create SnapshotManager
    let snapshot_manager = SnapshotManager::new(project_root)?;
    let snapshot_manager = Arc::new(snapshot_manager);

    // Create Watchdog
    let watchdog_config = WatchdogConfig::default();
    let mut watchdog = Watchdog::new(lock_manager.clone(), watchdog_config);
    watchdog.start()?;
    let watchdog = Arc::new(RwLock::new(watchdog));

    // Store in global state
    manager.projects.insert(project_id.to_string(), ProjectFileAccess {
        lock_manager,
        snapshot_manager,
        watchdog,
    });

    info!(
        project_id = project_id,
        project_root = %project_root.display(),
        "File access control system initialized"
    );

    Ok(())
}

/// Get the FileLockManager for a project
pub async fn get_lock_manager(project_id: &str) -> ErgataiResult<Arc<FileLockManager>> {
    let manager = file_access_manager();
    let manager = manager.read().await;

    manager
        .projects
        .get(project_id)
        .map(|p| p.lock_manager.clone())
        .ok_or_else(|| {
            crate::error::ErgataiError::NotFound(format!(
                "File access control not initialized for project: {}",
                project_id
            ))
        })
}

/// Get the SnapshotManager for a project
pub async fn get_snapshot_manager(project_id: &str) -> ErgataiResult<Arc<SnapshotManager>> {
    let manager = file_access_manager();
    let manager = manager.read().await;

    manager
        .projects
        .get(project_id)
        .map(|p| p.snapshot_manager.clone())
        .ok_or_else(|| {
            crate::error::ErgataiError::NotFound(format!(
                "File access control not initialized for project: {}",
                project_id
            ))
        })
}

/// Get the Watchdog for a project
pub async fn get_watchdog(project_id: &str) -> ErgataiResult<Arc<RwLock<Watchdog>>> {
    let manager = file_access_manager();
    let manager = manager.read().await;

    manager
        .projects
        .get(project_id)
        .map(|p| p.watchdog.clone())
        .ok_or_else(|| {
            crate::error::ErgataiError::NotFound(format!(
                "File access control not initialized for project: {}",
                project_id
            ))
        })
}

/// Shutdown file access control for a project
pub async fn shutdown_file_access(project_id: &str) -> ErgataiResult<()> {
    let manager = file_access_manager();
    let mut manager = manager.write().await;

    if let Some(project) = manager.projects.remove(project_id) {
        // Stop watchdog
        let mut watchdog = project.watchdog.write().await;
        watchdog.stop()?;

        info!(project_id = project_id, "File access control system shutdown");
    } else {
        warn!(project_id = project_id, "File access control not initialized for project");
    }

    Ok(())
}
