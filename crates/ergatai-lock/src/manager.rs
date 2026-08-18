//! Global file access manager for application-wide file access control
//!
//! Provides lazy initialization of FileLockManager, SnapshotManager, and Watchdog.
//! Similar to NatsManager, this provides a central point for file access control.
//!
//! Phase 9 adds an optional [`Enforcer`] that uses Linux fanotify to enforce
//! locks at the kernel level. The enforcer is created by
//! [`init_file_access_with_enforcer`]; the original [`init_file_access`] leaves
//! enforcement disabled (advisory-only mode) for backward compatibility.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;

use ergatai_error::ErgataiError;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::enforcer::{Enforcer, EnforcerConfig};
use crate::pid_resolver::PidResolver;
use crate::{FileLockManager, SnapshotManager, Watchdog, WatchdogConfig};
use ergatai_error::ErgataiResult;
use ergatai_nats::get_nats_connection;

/// Per-project file access control state
struct ProjectFileAccess {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
    /// Optional kernel-level enforcer (Phase 9). `None` in advisory-only mode.
    enforcer: Option<Arc<Enforcer>>,
}

/// Global file access state
struct FileAccessManagerState {
    projects: HashMap<String, ProjectFileAccess>,
}

static FILE_ACCESS_MANAGER: OnceLock<RwLock<FileAccessManagerState>> = OnceLock::new();

fn file_access_manager() -> &'static RwLock<FileAccessManagerState> {
    FILE_ACCESS_MANAGER.get_or_init(|| {
        RwLock::new(FileAccessManagerState {
            projects: HashMap::new(),
        })
    })
}

/// Initialize file access control for a project
///
/// Creates lock database, snapshot manager, and watchdog.
/// Idempotent - calling multiple times is safe.
/// If NATS is initialized, enables multi-agent approval flow.
///
/// M11 fix: NATS connection is fetched BEFORE acquiring the write lock
/// to avoid blocking all file access operations during NATS init.
pub async fn init_file_access(project_id: &str, project_root: &Path) -> ErgataiResult<()> {
    // M11 fix: Fetch NATS connection BEFORE acquiring write lock
    let nats_client = if let Some(conn) = get_nats_connection().await {
        info!(
            project_id = project_id,
            "NATS available, enabling multi-agent approval flow"
        );
        Some(Arc::new(conn.client().clone()))
    } else {
        warn!(
            project_id = project_id,
            "NATS not available, running in degraded mode (no multi-agent approval)"
        );
        None
    };

    let manager = file_access_manager();
    let mut manager = manager.write().await;

    // Check if already initialized
    if manager.projects.contains_key(project_id) {
        info!(
            project_id = project_id,
            "File access control already initialized"
        );
        return Ok(());
    }

    // Create lock database path
    let lock_db_path = project_root.join(".ergatai").join("locks.db");

    // Ensure .ergatai directory exists
    let lock_db_parent = lock_db_path.parent().ok_or_else(|| {
        ErgataiError::InvalidArgument(format!(
            "Invalid lock_db_path has no parent: {:?}",
            lock_db_path
        ))
    })?;
    tokio::fs::create_dir_all(lock_db_parent).await?;

    // Create FileLockManager with optional NATS client
    let lock_manager =
        FileLockManager::new(&lock_db_path, project_root.to_path_buf(), nats_client)?;

    // If NATS is available, subscribe to approval responses
    if let Err(e) = lock_manager.subscribe_to_nats().await {
        warn!(project_id = project_id, error = %e, "Failed to subscribe to NATS approval subjects, continuing without approval flow");
    }

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
    manager.projects.insert(
        project_id.to_string(),
        ProjectFileAccess {
            lock_manager,
            snapshot_manager,
            watchdog,
            enforcer: None, // Advisory-only mode; use init_file_access_with_enforcer for enforcement.
        },
    );

    info!(
        project_id = project_id,
        project_root = %project_root.display(),
        "File access control system initialized (advisory mode — no kernel enforcement)"
    );

    Ok(())
}

/// Initialize file access control with kernel-level enforcement (Phase 9).
///
/// Like [`init_file_access`], but also creates a fanotify-based [`Enforcer`]
/// that intercepts `open()` calls and denies access to locked files. The
/// `pid_resolver` maps kernel-reported PIDs to ergatai agent identities.
///
/// If fanotify initialization fails (non-Linux, no `CAP_SYS_ADMIN`, container),
/// the enforcer logs a warning and disables itself. Other components continue
/// to function normally.
///
/// Idempotent — calling multiple times is safe.
pub async fn init_file_access_with_enforcer(
    project_id: &str,
    project_root: &Path,
    pid_resolver: Arc<dyn PidResolver>,
) -> ErgataiResult<()> {
    // Fetch NATS connection BEFORE acquiring the write lock (M11 fix).
    let nats_client = if let Some(conn) = get_nats_connection().await {
        info!(
            project_id = project_id,
            "NATS available, enabling multi-agent approval flow + enforcement events"
        );
        Some(Arc::new(conn.client().clone()))
    } else {
        warn!(
            project_id = project_id,
            "NATS not available, running in degraded mode (no approval or enforcement events)"
        );
        None
    };

    let manager = file_access_manager();
    let mut manager = manager.write().await;

    if manager.projects.contains_key(project_id) {
        info!(
            project_id = project_id,
            "File access control already initialized"
        );
        return Ok(());
    }

    let lock_db_path = project_root.join(".ergatai").join("locks.db");

    let lock_db_parent = lock_db_path.parent().ok_or_else(|| {
        ErgataiError::InvalidArgument(format!(
            "Invalid lock_db_path has no parent: {:?}",
            lock_db_path
        ))
    })?;
    tokio::fs::create_dir_all(lock_db_parent).await?;

    let lock_manager = FileLockManager::new(
        &lock_db_path,
        project_root.to_path_buf(),
        nats_client.clone(),
    )?;

    if let Err(e) = lock_manager.subscribe_to_nats().await {
        warn!(project_id = project_id, error = %e, "Failed to subscribe to NATS approval subjects, continuing without approval flow");
    }

    let lock_manager = Arc::new(lock_manager);

    let snapshot_manager = SnapshotManager::new(project_root)?;
    let snapshot_manager = Arc::new(snapshot_manager);

    let watchdog_config = WatchdogConfig::default();
    let mut watchdog = Watchdog::new(lock_manager.clone(), watchdog_config);
    watchdog.start()?;
    let watchdog = Arc::new(RwLock::new(watchdog));

    // Create the fanotify enforcer. Fails open: if init fails, enforcer stays None.
    let enforcer = match Enforcer::start(
        project_root.to_path_buf(),
        project_id.to_string(),
        lock_manager.clone(),
        pid_resolver,
        nats_client,
        EnforcerConfig::default(),
    ) {
        Ok(e) => {
            let active = e.is_active();
            info!(
                project_id = project_id,
                active = active,
                "fanotify enforcer created (active = {})",
                active
            );
            Some(Arc::new(e))
        }
        Err(e) => {
            warn!(
                project_id = project_id,
                error = %e,
                "Enforcer init failed (continuing in advisory mode)"
            );
            None
        }
    };

    manager.projects.insert(
        project_id.to_string(),
        ProjectFileAccess {
            lock_manager,
            snapshot_manager,
            watchdog,
            enforcer,
        },
    );

    info!(
        project_id = project_id,
        project_root = %project_root.display(),
        "File access control system initialized (with kernel enforcement)"
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
            ergatai_error::ErgataiError::NotFound(format!(
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
            ergatai_error::ErgataiError::NotFound(format!(
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
            ergatai_error::ErgataiError::NotFound(format!(
                "File access control not initialized for project: {}",
                project_id
            ))
        })
}

/// Get the fanotify Enforcer for a project, if enforcement is enabled.
///
/// Returns `Ok(None)` if the project was initialized in advisory-only mode
/// (via [`init_file_access`]) or if the enforcer failed to initialize.
pub async fn get_enforcer(project_id: &str) -> ErgataiResult<Option<Arc<Enforcer>>> {
    let manager = file_access_manager();
    let manager = manager.read().await;

    Ok(manager
        .projects
        .get(project_id)
        .and_then(|p| p.enforcer.clone()))
}

/// Shutdown file access control for a project
pub async fn shutdown_file_access(project_id: &str) -> ErgataiResult<()> {
    let manager = file_access_manager();
    let mut manager = manager.write().await;

    if let Some(project) = manager.projects.remove(project_id) {
        // Stop enforcer first — it's the outermost layer (kernel interception).
        if let Some(enforcer) = project.enforcer.as_ref() {
            enforcer.stop().await;
        }

        // Stop NATS subscription.
        project.lock_manager.shutdown_nats_subscription();

        // Stop watchdog.
        let mut watchdog = project.watchdog.write().await;
        watchdog.stop()?;

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

/// Shutdown file access control for all projects.
///
/// Best-effort: logs but does not abort on individual failures. Called during
/// graceful shutdown to release locks, stop watchdogs, and close databases.
pub async fn shutdown_all_file_access() {
    let manager = file_access_manager();
    let project_ids: Vec<String> = {
        let state = manager.read().await;
        state.projects.keys().cloned().collect()
    };

    if project_ids.is_empty() {
        return;
    }

    info!(
        count = project_ids.len(),
        "Shutting down file access for all projects..."
    );
    for project_id in project_ids {
        if let Err(e) = shutdown_file_access(&project_id).await {
            warn!(
                project_id = %project_id,
                error = %e,
                "Failed to shutdown file access for project"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper: create a temp directory that is also a git repo (SnapshotManager needs it).
    fn setup_git_repo() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let project_root = temp_dir.path().to_path_buf();

        // Initialize a git repo so SnapshotManager::new succeeds
        git2::Repository::init(&project_root).unwrap();

        // Create an initial commit so HEAD exists
        let repo = git2::Repository::open(&project_root).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        (temp_dir, project_root)
    }

    /// Each test uses a unique project ID to avoid collisions via the global OnceLock.
    fn unique_project_id(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("test-mgr-{}-{}-{}", label, std::process::id(), n)
    }

    // ─── Getters on uninitialized projects ─────────────────────────

    #[tokio::test]
    async fn test_get_lock_manager_unknown_project() {
        let id = unique_project_id("unknown-lock");
        let result = get_lock_manager(&id).await;
        assert!(matches!(result, Err(ErgataiError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_get_snapshot_manager_unknown_project() {
        let id = unique_project_id("unknown-snap");
        let result = get_snapshot_manager(&id).await;
        assert!(matches!(result, Err(ErgataiError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_get_watchdog_unknown_project() {
        let id = unique_project_id("unknown-wd");
        let result = get_watchdog(&id).await;
        assert!(matches!(result, Err(ErgataiError::NotFound(_))));
    }

    // ─── Shutdown of uninitialized project ─────────────────────────

    #[tokio::test]
    async fn test_shutdown_unknown_project_is_noop() {
        let id = unique_project_id("unknown-shutdown");
        // Should NOT error — just logs a warning
        let result = shutdown_file_access(&id).await;
        assert!(result.is_ok());
    }

    // ─── Full lifecycle ────────────────────────────────────────────

    #[tokio::test]
    async fn test_init_file_access_creates_directory_and_managers() {
        let (temp, project_root) = setup_git_repo();
        let id = unique_project_id("init");

        // Before init: getters should fail
        assert!(get_lock_manager(&id).await.is_err());

        // Init
        init_file_access(&id, &project_root).await.unwrap();

        // .ergatai directory created
        let ergatai_dir = project_root.join(".ergatai");
        assert!(ergatai_dir.exists());
        assert!(ergatai_dir.join("locks.db").exists());

        // All 3 getters now succeed
        let _lm = get_lock_manager(&id).await.unwrap();
        let _sm = get_snapshot_manager(&id).await.unwrap();
        let _wd = get_watchdog(&id).await.unwrap();

        // Cleanup — stop watchdog and remove from global state
        shutdown_file_access(&id).await.unwrap();

        // After shutdown: getters should fail again
        assert!(get_lock_manager(&id).await.is_err());

        drop(temp);
    }

    #[tokio::test]
    async fn test_init_file_access_idempotent() {
        let (temp, project_root) = setup_git_repo();
        let id = unique_project_id("idempotent");

        // Call twice — second call should be a no-op, not error
        init_file_access(&id, &project_root).await.unwrap();
        init_file_access(&id, &project_root).await.unwrap();

        // Still works after second init
        let _lm = get_lock_manager(&id).await.unwrap();

        shutdown_file_access(&id).await.unwrap();
        drop(temp);
    }

    #[tokio::test]
    async fn test_multiple_projects_independent() {
        let (temp_a, root_a) = setup_git_repo();
        let (temp_b, root_b) = setup_git_repo();
        let id_a = unique_project_id("multi-a");
        let id_b = unique_project_id("multi-b");

        init_file_access(&id_a, &root_a).await.unwrap();
        init_file_access(&id_b, &root_b).await.unwrap();

        // Both can be fetched
        let lm_a = get_lock_manager(&id_a).await.unwrap();
        let lm_b = get_lock_manager(&id_b).await.unwrap();

        // They are different Arc instances pointing to different managers
        assert!(!Arc::ptr_eq(&lm_a, &lm_b));

        // Shutdown one doesn't affect the other
        shutdown_file_access(&id_a).await.unwrap();
        assert!(get_lock_manager(&id_a).await.is_err());
        assert!(get_lock_manager(&id_b).await.is_ok());

        shutdown_file_access(&id_b).await.unwrap();
        drop(temp_a);
        drop(temp_b);
    }
}
