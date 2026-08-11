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
                    event = event_rx.recv() => {
                        match event {
                            Some(event) => {
                                if let Err(e) = Self::handle_event(&lock_manager, &project_root, event).await {
                                    error!("Failed to handle file system event: {}", e);
                                }
                            }
                            None => {
                                // Event channel closed (notify watcher dropped), shut down
                                info!("FileSystemWatcher event channel closed, shutting down");
                                break;
                            }
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
