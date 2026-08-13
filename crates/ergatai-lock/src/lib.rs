//! File access control module.
//!
//! Provides zero-trust file access control for multi-agent collaboration.
//! Two-tier token system: SystemToken (admission) + FileToken (operation permissions).
//! SQLite-backed lock management with WAL mode for concurrent performance.
//! Git-based snapshots for Copy-on-Write semantics (TOCTOU prevention).
//! Watchdog for token expiration and heartbeat monitoring (Phase 5).
//! File system watcher for detecting unauthorized modifications (Phase 6).
//! File events consumer for handling file.ready and file.error events (Phase 7).
//! NATS-based lock waiting queue for blocking lock acquisition (Phase 8).

pub mod audit;
pub mod config;
pub mod conflict_arbitration;
pub mod file_events_consumer;
pub mod lock_manager;
pub mod lock_mode;
pub mod lock_wait_consumer;
pub mod lock_waiter;
pub mod manager;
pub mod performance;
pub mod renewal;
pub mod sensitive_paths;
pub mod snapshot;
pub mod token;
pub mod watchdog;
pub mod watcher;

#[cfg(test)]
mod multi_agent_tests;

pub use audit::{AuditEntry, AuditManager, FileAccessStats, SecurityReport};
pub use config::{ConfigManager, FileAccessConfig};
pub use file_events_consumer::{FileEvent, FileEventsConsumer};
pub use lock_manager::FileLockManager;
pub use lock_mode::LockModeManager;
pub use lock_wait_consumer::LockWaitConsumer;
pub use lock_waiter::{
    LockCancelRequest, LockGrantedNotification, LockPriority, LockReleaseNotification,
    LockWaitRequest,
};
pub use manager::{
    get_lock_manager, get_snapshot_manager, get_watchdog, init_file_access, shutdown_file_access,
};
pub use performance::{AsyncLockQueue, AsyncLockRequest, BatchOperations, LockCache};
pub use renewal::RenewalManager;
pub use snapshot::SnapshotManager;
pub use token::{FileLock, FileMode, FileToken, SystemToken, TokenId, TokenStatus};
pub use watchdog::{Watchdog, WatchdogConfig};
pub use watcher::FileSystemWatcher;
