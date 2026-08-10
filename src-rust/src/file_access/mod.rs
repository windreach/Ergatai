//! File access control module.
//!
//! Provides zero-trust file access control for multi-agent collaboration.
//! Two-tier token system: SystemToken (admission) + FileToken (operation permissions).
//! SQLite-backed lock management with WAL mode for concurrent performance.
//! Git-based snapshots for Copy-on-Write semantics (TOCTOU prevention).
//! Watchdog for token expiration and heartbeat monitoring (Phase 5).
//! File system watcher for detecting unauthorized modifications (Phase 6).
//! File events consumer for handling file.ready and file.error events (Phase 7).

pub mod lock_manager;
pub mod token;
pub mod snapshot;
pub mod watchdog;
pub mod watcher;
pub mod file_events_consumer;
pub mod manager;
pub mod sensitive_paths;
pub mod conflict_arbitration;
pub mod lock_mode;
pub mod renewal;
pub mod audit;
pub mod performance;

pub use lock_manager::FileLockManager;
pub use token::{FileLock, FileMode, FileToken, SystemToken, TokenId, TokenStatus};
pub use snapshot::SnapshotManager;
pub use watchdog::{Watchdog, WatchdogConfig};
pub use watcher::FileSystemWatcher;
pub use file_events_consumer::{FileEventsConsumer, FileEvent, FileReadyPayload};
pub use lock_mode::LockModeManager;
pub use renewal::RenewalManager;
pub use audit::{AuditManager, AuditEntry, FileAccessStats, SecurityReport};
pub use performance::{LockCache, BatchOperations, AsyncLockQueue, AsyncLockRequest};
pub use manager::{init_file_access, get_lock_manager, get_snapshot_manager, get_watchdog, shutdown_file_access};
