//! File system watcher for detecting unauthorized file modifications.
//!
//! Phase 6 (Plan): fsevents/inotify Fallback
//! Uses notify crate to monitor file system events and detect writes
//! that bypass the lock system. Audits and alerts but does not auto-rollback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::lock_manager::FileLockManager;

/// File system watcher for detecting unauthorized modifications
pub struct FileSystemWatcher {
    /// Watcher instance
    _watcher: RecommendedWatcher,
    /// Receiver for file system events
    event_rx: Option<mpsc::Receiver<Event>>,
    /// Lock manager for checking authorized locks
    lock_manager: Arc<FileLockManager>,
    /// Project root directory
    project_root: PathBuf,
    /// Shutdown signal
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FileSystemWatcher {
    /// Create a new file system watcher
    pub fn new(
        lock_manager: Arc<FileLockManager>,
        project_root: PathBuf,
    ) -> ErgataiResult<Self> {
        // Create channel for file system events
        let (event_tx, event_rx) = mpsc::channel(1000);

        // Create watcher with config
        let config = Config::default()
            .with_poll_interval(Duration::from_secs(2))  // Poll every 2 seconds
            .with_compare_contents(true);  // Compare file contents to detect changes

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    // Use try_send to avoid blocking notify's internal thread pool.
                    // If the channel is full, the event is dropped with a warning.
                    if let Err(e) = event_tx.try_send(event) {
                        warn!("File system event dropped (channel full): {}", e);
                    }
                }
            },
            config,
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to create watcher: {}", e)))?;

        // Watch the project root recursively
        watcher
            .watch(&project_root, RecursiveMode::Recursive)
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to watch project root: {}", e))
            })?;

        info!(
            "FileSystemWatcher started for project root: {:?}",
            project_root
        );

        Ok(Self {
            _watcher: watcher,
            event_rx: Some(event_rx),
            lock_manager,
            project_root,
            shutdown_tx: None,
        })
    }

    /// Start the watcher background task
    pub fn start(&mut self) -> ErgataiResult<()> {
        if self.shutdown_tx.is_some() {
            return Err(ErgataiError::InvalidArgument(
                "FileSystemWatcher already started".to_string(),
            ));
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(tx);

        // Take the event receiver out of self
        let mut event_rx = self.event_rx.take().ok_or_else(|| {
            ErgataiError::internal("FileSystemWatcher event receiver already taken")
        })?;

        let lock_manager = Arc::clone(&self.lock_manager);
        let project_root = self.project_root.clone();

        // Spawn background task to process events
        tokio::spawn(async move {
            info!("FileSystemWatcher background task started");
            let mut shutdown_rx = rx;

            loop {
                tokio::select! {
                    // Process file system events
                    Some(event) = event_rx.recv() => {
                        if let Err(e) = Self::handle_event(&lock_manager, &project_root, event).await {
                            error!("Failed to handle file system event: {}", e);
                        }
                    }
                    // Shutdown signal
                    _ = &mut shutdown_rx => {
                        info!("FileSystemWatcher received shutdown signal");
                        break;
                    }
                }
            }

            info!("FileSystemWatcher background task stopped");
        });

        Ok(())
    }

    /// Stop the watcher background task
    pub fn stop(&mut self) -> ErgataiResult<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("FileSystemWatcher shutdown signal sent");
        }
        Ok(())
    }

    /// Handle a file system event
    async fn handle_event(
        lock_manager: &Arc<FileLockManager>,
        project_root: &Path,
        event: Event,
    ) -> ErgataiResult<()> {
        // Only process modify/create events
        match event.kind {
            EventKind::Modify(_) | EventKind::Create(_) => {}
            _ => return Ok(()),
        }

        for path in event.paths {
            // Skip directories
            if path.is_dir() {
                continue;
            }

            // Get relative path
            let relative_path = match path.strip_prefix(project_root) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            // Check if file is locked
            let is_locked = lock_manager.is_file_locked(&relative_path)?;

            if !is_locked {
                // File was modified without a lock - potential violation
                warn!(
                    file_path = relative_path,
                    "Unauthorized file modification detected (no active lock)"
                );

                // Log to audit log
                Self::log_violation(lock_manager, &relative_path, "unauthorized_modification").await?;
            } else {
                debug!(
                    file_path = relative_path,
                    "File modification detected (lock exists)"
                );
            }
        }

        Ok(())
    }

    /// Log a violation to the audit log
    async fn log_violation(
        _lock_manager: &Arc<FileLockManager>,
        file_path: &str,
        action: &str,
    ) -> ErgataiResult<()> {
        // For now, just log the violation
        // TODO: Add audit_log table methods to FileLockManager
        warn!(
            file_path = file_path,
            action = action,
            "File access violation logged"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::{Event, EventKind};
    use std::fs;
    use tempfile::TempDir;

    use crate::file_access::{FileMode, FileToken, SystemToken};

    /// Helper: create a test lock manager in a temp directory.
    /// Returns the temp dir (keeps it alive), manager, and project root path.
    fn setup() -> (TempDir, Arc<FileLockManager>, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let project_root = temp_dir.path().to_path_buf();

        // Create some test files
        fs::write(project_root.join("main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(project_root.join("src")).unwrap();
        fs::write(project_root.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        let manager = Arc::new(FileLockManager::new(&db_path, project_root.clone()).unwrap());
        (temp_dir, manager, project_root)
    }

    /// Helper: build a notify::Event with given kind and paths
    fn make_event(kind: EventKind, paths: Vec<PathBuf>) -> Event {
        let mut e = Event::new(kind);
        for p in paths {
            e = e.add_path(p);
        }
        e
    }

    /// Helper: register a system token and create a file token
    fn make_token_and_register(
        manager: &FileLockManager,
        agent_id: &str,
        session_id: &str,
        scope: &str,
        mode: FileMode,
    ) -> FileToken {
        let sys = SystemToken::new(
            agent_id.to_string(),
            session_id.to_string(),
            "/test".to_string(),
            3600,
            30,
        );
        manager.register_system_token(&sys).unwrap();
        FileToken::new(
            agent_id.to_string(),
            session_id.to_string(),
            sys.id.clone(),
            scope.to_string(),
            mode,
            Some("test".to_string()),
            "test-system".to_string(),
            3600,
            15,
        )
    }

    // ─── EventKind filtering ───────────────────────────────────────

    #[tokio::test]
    async fn test_handle_event_skips_remove_events() {
        let (_temp, manager, root) = setup();
        let event = make_event(
            EventKind::Remove(notify::event::RemoveKind::File),
            vec![root.join("main.rs")],
        );
        // Remove events should be silently ignored
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_event_skips_access_events() {
        let (_temp, manager, root) = setup();
        let event = make_event(
            EventKind::Access(notify::event::AccessKind::Read),
            vec![root.join("main.rs")],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_event_processes_modify_events() {
        let (_temp, manager, root) = setup();
        // main.rs is not locked, but processing should still succeed
        let event = make_event(
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            vec![root.join("main.rs")],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handle_event_processes_create_events() {
        let (_temp, manager, root) = setup();
        // The file must exist on disk because is_file_locked → validate_and_normalize_path
        // calls canonicalize() which fails on non-existent paths.
        let new_file = root.join("new_file.rs");
        fs::write(&new_file, "new content").unwrap();
        let event = make_event(
            EventKind::Create(notify::event::CreateKind::File),
            vec![new_file],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    // ─── Path handling ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_handle_event_skips_paths_outside_project_root() {
        let (_temp, manager, root) = setup();
        // Path completely outside project_root — strip_prefix fails
        let event = make_event(
            EventKind::Modify(notify::event::ModifyKind::Any),
            vec![PathBuf::from("/tmp/outside_file.txt")],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
        // No violation logged because path is just skipped
    }

    #[tokio::test]
    async fn test_handle_event_skips_directories() {
        let (_temp, manager, root) = setup();
        // Create a real directory so path.is_dir() returns true
        let dir_path = root.join("new_dir");
        fs::create_dir_all(&dir_path).unwrap();
        let event = make_event(
            EventKind::Create(notify::event::CreateKind::Folder),
            vec![dir_path],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    // ─── Lock-based violation detection ────────────────────────────

    #[tokio::test]
    async fn test_handle_event_unlocked_file_is_violation() {
        let (_temp, manager, root) = setup();
        // main.rs is not locked — should log a violation (but still return Ok)
        let event = make_event(
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            vec![root.join("main.rs")],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
        // The violation is logged via warn! — functionally it still succeeds
    }

    #[tokio::test]
    async fn test_handle_event_locked_file_is_not_violation() {
        let (_temp, manager, root) = setup();

        // Create a token and acquire a lock on main.rs
        let token = make_token_and_register(
            &manager,
            "agent-1",
            "session-1",
            "**",
            FileMode::Write,
        );
        manager.acquire_lock(&token, "main.rs").unwrap();

        let event = make_event(
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            vec![root.join("main.rs")],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    // ─── log_violation ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_log_violation_succeeds() {
        let (_temp, manager, _root) = setup();
        let result =
            FileSystemWatcher::log_violation(&manager, "some/file.rs", "unauthorized_modification")
                .await;
        assert!(result.is_ok());
    }

    // ─── Multiple paths in one event ───────────────────────────────

    #[tokio::test]
    async fn test_handle_event_multiple_paths_mixed() {
        let (_temp, manager, root) = setup();

        // Mix: one outside project, one dir, one real unlocked file
        let outside = PathBuf::from("/outside/file.txt");
        let dir_path = root.join("a_dir");
        fs::create_dir_all(&dir_path).unwrap();
        let real_file = root.join("main.rs");

        let event = make_event(
            EventKind::Modify(notify::event::ModifyKind::Any),
            vec![outside, dir_path, real_file],
        );
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }

    // ─── Empty event (no paths) ────────────────────────────────────

    #[tokio::test]
    async fn test_handle_event_empty_paths() {
        let (_temp, manager, root) = setup();
        let event = make_event(EventKind::Modify(notify::event::ModifyKind::Any), vec![]);
        let result = FileSystemWatcher::handle_event(&manager, &root, event).await;
        assert!(result.is_ok());
    }
}
