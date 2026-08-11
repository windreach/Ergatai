//! SQLite-based file lock manager.
//!
//! Uses BEGIN IMMEDIATE + unique index constraints for atomicity.
//! WAL mode enabled for concurrent read performance.

use crate::error::ErgataiError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::token::{FileLock, FileMode, FileToken, SystemToken, TokenId, TokenStatus};

/// File lock manager backed by SQLite.
///
/// Thread-safe via internal Mutex. All operations use BEGIN IMMEDIATE for atomicity.
pub struct FileLockManager {
    /// SQLite connection (wrapped in Mutex for thread safety).
    conn: Arc<Mutex<Connection>>,
    /// Project root directory (for path canonicalization).
    project_root: PathBuf,
    /// Cached canonical project root (M2 fix: avoid repeated I/O).
    project_root_canonical: PathBuf,
    /// Waiters for READ_LATEST (file_path → list of notification channels).
    waiters: Arc<Mutex<HashMap<String, Vec<oneshot::Sender<Result<(), String>>>>>>,
}

impl FileLockManager {
    /// Create a new lock manager with the given database path.
    ///
    /// Enables WAL mode and creates tables if they don't exist.
    pub fn new(db_path: &Path, project_root: PathBuf) -> Result<Self, ErgataiError> {
        info!("Initializing FileLockManager at {:?}", db_path);

        let conn = Connection::open(db_path).map_err(|e| {
            ErgataiError::internal(format!("Failed to open lock database: {}", e))
        })?;

        // Enable WAL mode for better concurrent performance (C3 fix)
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA cache_size=-64000;
            PRAGMA foreign_keys=ON;
            ",
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to set pragmas: {}", e)))?;

        // Create tables
        Self::create_tables(&conn)?;

        // Cache canonical project root (M2 fix: avoid repeated I/O)
        let project_root_canonical = project_root.canonicalize().map_err(|e| {
            ErgataiError::internal(format!(
                "Failed to canonicalize project root: {}",
                e
            ))
        })?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            project_root,
            project_root_canonical,
            waiters: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Create the lock database tables.
    fn create_tables(conn: &Connection) -> Result<(), ErgataiError> {
        conn.execute_batch(
            "
            -- System tokens (agent admission)
            CREATE TABLE IF NOT EXISTS system_tokens (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL UNIQUE,
                project_root TEXT NOT NULL,
                issued_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                heartbeat_interval_secs INTEGER NOT NULL,
                heartbeat_at TEXT NOT NULL,
                status TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_system_tokens_session
                ON system_tokens(session_id);

            -- File locks
            CREATE TABLE IF NOT EXISTS file_locks (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                mode TEXT NOT NULL,
                scope TEXT,
                token_id TEXT NOT NULL,
                reason TEXT,
                approved_by TEXT,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                heartbeat_interval_secs INTEGER NOT NULL,
                heartbeat_at TEXT NOT NULL,
                status TEXT NOT NULL
            );

            -- Unique constraint: only one WRITE per file (enforced at DB level)
            CREATE UNIQUE INDEX IF NOT EXISTS idx_file_locks_write_unique
                ON file_locks(file_path)
                WHERE mode = 'WRITE' AND status = 'ACTIVE';

            CREATE INDEX IF NOT EXISTS idx_file_locks_path
                ON file_locks(file_path);
            CREATE INDEX IF NOT EXISTS idx_file_locks_agent
                ON file_locks(agent_id, session_id);

            -- Audit log
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                action TEXT NOT NULL,
                file_path TEXT,
                mode TEXT,
                reason TEXT,
                details TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_audit_log_time
                ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_log_agent
                ON audit_log(agent_id);

            -- File tokens (per-file operation permissions)
            CREATE TABLE IF NOT EXISTS file_tokens (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                system_token_id TEXT NOT NULL,
                scope TEXT,
                mode TEXT NOT NULL,
                reason TEXT,
                approved_by TEXT,
                issued_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                heartbeat_interval_secs INTEGER NOT NULL,
                heartbeat_at TEXT NOT NULL,
                status TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_file_tokens_session
                ON file_tokens(session_id);
            CREATE INDEX IF NOT EXISTS idx_file_tokens_status
                ON file_tokens(status);

            -- File snapshots (for READ_HISTORY and rollback)
            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                git_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                created_by TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_snapshots_path
                ON snapshots(file_path);
            CREATE INDEX IF NOT EXISTS idx_snapshots_time
                ON snapshots(created_at);
            ",
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to create tables: {}", e)))?;

        Ok(())
    }

    /// Register a new system token.
    pub fn register_system_token(&self, token: &SystemToken) -> Result<(), ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute(
            "INSERT INTO system_tokens (
                id, agent_id, session_id, project_root,
                issued_at, expires_at, heartbeat_interval_secs, heartbeat_at, status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                token.id.as_str(),
                token.agent_id,
                token.session_id,
                token.project_root,
                token.issued_at.to_rfc3339(),
                token.expires_at.to_rfc3339(),
                token.heartbeat_interval_secs as i64,
                token.heartbeat_at.to_rfc3339(),
                token.status.to_string(),
            ],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to insert system token: {}", e)))?;

        debug!("Registered system token {} for agent {}", token.id, token.agent_id);
        Ok(())
    }

    /// Get a system token by session ID.
    pub fn get_system_token(&self, session_id: &str) -> Result<Option<SystemToken>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, project_root,
                        issued_at, expires_at, heartbeat_interval_secs, heartbeat_at, status
                 FROM system_tokens
                 WHERE session_id = ?1",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let result = match stmt.query_row(params![session_id], |row| {
                Ok(SystemToken {
                    id: TokenId::from_string(row.get(0)?),
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    project_root: row.get(3)?,
                    issued_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    heartbeat_interval_secs: row.get::<_, i64>(6)? as u64,
                    heartbeat_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    status: match row.get::<_, String>(8)?.as_str() {
                        "ACTIVE" => TokenStatus::Active,
                        "UPGRADING" => TokenStatus::Upgrading,
                        "EXPIRED" => TokenStatus::Expired,
                        "REVOKED" => TokenStatus::Revoked,
                        _ => TokenStatus::Expired,
                    },
                })
            }) {
                Ok(token) => Some(token),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    return Err(ErgataiError::internal(format!("Failed to query token by session: {}", e)));
                }
            };

        Ok(result)
    }

    /// Acquire a file lock.
    ///
    /// Uses BEGIN IMMEDIATE for atomicity. Checks for conflicts via unique index.
    /// M1 fix: Path validation happens before acquiring the mutex to reduce lock contention.
    pub fn acquire_lock(&self, token: &FileToken, file_path: &str) -> Result<(), ErgataiError> {
        // Validate and normalize path BEFORE acquiring lock (M1 fix)
        let normalized_path = self.validate_and_normalize_path(file_path)?;

        // Check if path is within token scope (M5 fix) BEFORE acquiring lock
        if !token.matches_path(&normalized_path) {
            return Err(ErgataiError::PermissionDenied(format!(
                "File {} is outside token scope {}",
                file_path, token.scope
            )));
        }

        // Check if path is sensitive and requires ADMIN permission
        if crate::file_access::sensitive_paths::is_sensitive_path(&normalized_path) {
            if token.mode != FileMode::Admin {
                return Err(ErgataiError::PermissionDenied(format!(
                    "File {} is a sensitive path and requires ADMIN permission (current mode: {:?})",
                    file_path, token.mode
                )));
            }
        }

        // Now acquire the lock for database operations only
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        // BEGIN IMMEDIATE (not EXCLUSIVE) for better concurrency
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Check for WRITE conflict (unique index will enforce this)
        if token.mode == FileMode::Write {
            // Get conflict information for arbitration
            let conflict_info = match conn
                .query_row(
                    "SELECT agent_id, session_id, token_id, reason FROM file_locks
                     WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'
                     LIMIT 1",
                    params![normalized_path],
                    |row| {
                        Ok(crate::file_access::conflict_arbitration::ConflictInfo {
                            file_path: normalized_path.clone(),
                            current_holder: crate::file_access::conflict_arbitration::LockHolderInfo {
                                agent_id: row.get(0)?,
                                session_id: row.get(1)?,
                                token_id: row.get(2)?,
                                priority: None, // TODO: Get from task metadata
                                reason: row.get(3)?,
                            },
                            new_requester: crate::file_access::conflict_arbitration::LockHolderInfo {
                                agent_id: token.agent_id.clone(),
                                session_id: token.session_id.clone(),
                                token_id: token.id.as_str().to_string(),
                                priority: None, // TODO: Get from task metadata
                                reason: token.reason.clone(),
                            },
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        })
                    },
                ) {
                    Ok(info) => Some(info),
                    Err(rusqlite::Error::QueryReturnedNoRows) => None,
                    Err(e) => {
                        conn.execute_batch("ROLLBACK").ok();
                        return Err(ErgataiError::internal(format!(
                            "Failed to check WRITE conflict (DB error): {}", e
                        )));
                    }
                };

            if let Some(conflict) = conflict_info {
                // Arbitrate the conflict
                let decision = crate::file_access::conflict_arbitration::arbitrate_conflict(&conflict);

                match decision {
                    crate::file_access::conflict_arbitration::ArbitrationDecision::KeepWithCurrentHolder => {
                        conn.execute_batch("ROLLBACK").ok();
                        return Err(ErgataiError::LockConflict(format!(
                            "File {} is already locked for writing by {} (arbitration: keep with current holder)",
                            file_path, conflict.current_holder.agent_id
                        )));
                    }
                    crate::file_access::conflict_arbitration::ArbitrationDecision::GrantToNewRequester => {
                        // Preempt current holder: expire their lock
                        conn.execute(
                            "UPDATE file_locks SET status = 'EXPIRED'
                             WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'",
                            params![normalized_path],
                        ).ok();

                        tracing::info!(
                            file_path = %normalized_path,
                            preempted_agent = %conflict.current_holder.agent_id,
                            new_agent = %conflict.new_requester.agent_id,
                            "WRITE lock preempted: new requester has higher priority"
                        );
                    }
                    crate::file_access::conflict_arbitration::ArbitrationDecision::RejectBoth => {
                        conn.execute_batch("ROLLBACK").ok();
                        return Err(ErgataiError::LockConflict(format!(
                            "File {} lock conflict rejected by arbitration",
                            file_path
                        )));
                    }
                }
            }
        }

        // Insert lock record
        let now = Utc::now();
        conn.execute(
            "INSERT INTO file_locks (
                id, file_path, agent_id, session_id, mode, scope, token_id,
                reason, approved_by, created_at, expires_at,
                heartbeat_interval_secs, heartbeat_at, status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                uuid::Uuid::new_v4().to_string(),
                normalized_path,
                token.agent_id,
                token.session_id,
                token.mode.to_string(),
                token.scope,
                token.id.as_str(),
                token.reason,
                token.approved_by,
                now.to_rfc3339(),
                token.expires_at.to_rfc3339(),
                token.heartbeat_interval_secs as i64,
                now.to_rfc3339(),
                TokenStatus::Active.to_string(),
            ],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            // Check if it's a unique constraint violation
            if e.to_string().contains("UNIQUE constraint failed") {
                ErgataiError::LockConflict(format!(
                    "File {} is already locked for writing",
                    file_path
                ))
            } else {
                ErgataiError::internal(format!("Failed to insert lock: {}", e))
            }
        })?;

        // COMMIT (including audit log in same transaction to avoid reentrant lock)
        let now_audit = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![now_audit, token.agent_id, token.session_id, "LOCK_ACQUIRED", normalized_path, token.mode.to_string(), token.reason],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            ErgataiError::internal(format!("Failed to log audit: {}", e))
        })?;

        conn.execute_batch("COMMIT")
            .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

        info!(
            "Agent {} acquired {:?} lock on {}",
            token.agent_id, token.mode, file_path
        );
        Ok(())
    }

    /// Release a file lock.
    ///
    /// Automatically notifies waiters (READ_LATEST) after releasing the lock.
    pub async fn release_lock(&self, token_id: &str, file_path: &str) -> Result<(), ErgataiError> {
        let normalized_path = self.validate_and_normalize_path(file_path)?;

        // Use a block to ensure conn is dropped before async call
        let release_result = {
            let conn = self.conn.lock().map_err(|e| {
                ErgataiError::internal(format!("Failed to acquire lock: {}", e))
            })?;

            conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Get lock info for audit
        let lock_info: Option<(String, String)> = match conn
            .query_row(
                "SELECT agent_id, session_id FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'",
                params![token_id, normalized_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ) {
                Ok(info) => Some(info),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    conn.execute_batch("ROLLBACK").ok();
                    return Err(ErgataiError::internal(format!("Failed to query lock info for release: {}", e)));
                }
            };

        if let Some((agent_id, session_id)) = lock_info {
            // Mark lock as expired
            conn.execute(
                "UPDATE file_locks SET status = 'EXPIRED'
                 WHERE token_id = ?1 AND file_path = ?2",
                params![token_id, normalized_path],
            )
            .map_err(|e| {
                conn.execute_batch("ROLLBACK").ok();
                ErgataiError::internal(format!("Failed to update lock: {}", e))
            })?;

            // Log to audit IN SAME TRANSACTION (avoid reentrant lock)
            let now_audit = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![now_audit, agent_id, session_id, "LOCK_RELEASED", normalized_path, Option::<String>::None, Option::<String>::None],
            )
            .map_err(|e| {
                conn.execute_batch("ROLLBACK").ok();
                ErgataiError::internal(format!("Failed to log audit: {}", e))
            })?;

            conn.execute_batch("COMMIT")
                .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

            info!("Released lock on {}", file_path);
            Ok(())
        } else {
            conn.execute_batch("ROLLBACK").ok();
            Err(ErgataiError::NotFound(format!(
                "No active lock found for token {} on file {}",
                token_id, file_path
            )))
        }
        }; // conn is dropped here

        // Notify waiters after releasing the mutex
        release_result?;
        self.notify_file_ready(&normalized_path).await?;

        Ok(())
    }

    /// Check if a file is locked for writing.
    pub fn is_file_locked(&self, file_path: &str) -> Result<bool, ErgataiError> {
        let normalized_path = self.validate_and_normalize_path(file_path)?;

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let is_locked: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM file_locks
                 WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'",
                params![normalized_path],
                |row| row.get(0),
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to query lock: {}", e)))?;

        Ok(is_locked)
    }

    /// Update heartbeat for a token.
    ///
    /// M3 fix: Uses transaction to ensure atomicity across both tables.
    pub fn update_heartbeat(&self, token_id: &str) -> Result<(), ErgataiError> {
        let now = Utc::now().to_rfc3339();

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        // Use transaction for atomicity (M3 fix)
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Update in both tables (system_tokens and file_locks)
        conn.execute(
            "UPDATE system_tokens SET heartbeat_at = ?1 WHERE id = ?2",
            params![now, token_id],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            ErgataiError::internal(format!("Failed to update system_tokens heartbeat: {}", e))
        })?;

        conn.execute(
            "UPDATE file_locks SET heartbeat_at = ?1 WHERE token_id = ?2",
            params![now, token_id],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            ErgataiError::internal(format!("Failed to update file_locks heartbeat: {}", e))
        })?;

        conn.execute_batch("COMMIT")
            .map_err(|e| {
                conn.execute_batch("ROLLBACK").ok();
                ErgataiError::internal(format!("Failed to commit heartbeat update: {}", e))
            })?;

        debug!("Updated heartbeat for token {}", token_id);
        Ok(())
    }

    /// Validate and normalize a file path (H2 fix).
    ///
    /// - Canonicalizes the path
    /// - Ensures it's within project root
    /// - Returns relative path from project root
    /// - M2 fix: Uses cached project_root_canonical to avoid repeated I/O
    fn validate_and_normalize_path(&self, file_path: &str) -> Result<String, ErgataiError> {
        let full_path = self.project_root.join(file_path);

        // Canonicalize (resolves symlinks, .., etc.)
        let canonical = full_path.canonicalize().map_err(|e| {
            ErgataiError::InvalidPath(format!(
                "Failed to canonicalize path {}: {}",
                file_path, e
            ))
        })?;

        // Use cached project_root_canonical (M2 fix: avoid repeated canonicalize)
        if !canonical.starts_with(&self.project_root_canonical) {
            return Err(ErgataiError::PermissionDenied(format!(
                "Path {} escapes project root",
                file_path
            )));
        }

        // Return relative path from project root
        let relative = canonical
            .strip_prefix(&self.project_root_canonical)
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to compute relative path: {}", e))
            })?;

        // Convert to string (use forward slashes even on Windows)
        let path_str = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        Ok(path_str)
    }

    /// Log an action to the audit log.
    pub fn log_audit(
        &self,
        agent_id: &str,
        session_id: &str,
        action: &str,
        file_path: Option<&str>,
        mode: Option<&str>,
        reason: Option<&str>,
    ) -> Result<(), ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![now, agent_id, session_id, action, file_path, mode, reason],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to log audit: {}", e)))?;

        Ok(())
    }

    /// Clean up old audit log entries (M5 fix).
    ///
    /// Removes entries older than the specified number of days.
    /// Returns the number of entries deleted.
    pub fn cleanup_old_audit_logs(&self, days_to_keep: u32) -> Result<usize, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let cutoff = Utc::now() - chrono::Duration::days(days_to_keep as i64);
        let deleted = conn
            .execute(
                "DELETE FROM audit_log WHERE timestamp < ?1",
                params![cutoff.to_rfc3339()],
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to cleanup audit log: {}", e)))?;

        info!(
            "Cleaned up {} old audit log entries (older than {} days)",
            deleted, days_to_keep
        );
        Ok(deleted)
    }

    // ============================================================
    // Phase 5: Watchdog support methods
    // ============================================================

    /// Get all active tokens (for watchdog monitoring).
    ///
    /// Returns all tokens with status "ACTIVE".
    pub fn get_active_tokens(&self) -> Result<Vec<SystemToken>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, project_root, issued_at, expires_at,
                        heartbeat_interval_secs, heartbeat_at, status
                 FROM system_tokens
                 WHERE status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare statement: {}", e)))?;

        let tokens = stmt
            .query_map([], |row| {
                Ok(SystemToken {
                    id: TokenId::from_string(row.get(0)?),
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    project_root: row.get(3)?,
                    issued_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    heartbeat_interval_secs: row.get::<_, i64>(6)? as u64,
                    heartbeat_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    status: match row.get::<_, String>(8)?.as_str() {
                        "ACTIVE" => TokenStatus::Active,
                        "UPGRADING" => TokenStatus::Upgrading,
                        "EXPIRED" => TokenStatus::Expired,
                        "REVOKED" => TokenStatus::Revoked,
                        _ => TokenStatus::Expired,
                    },
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query tokens: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect tokens: {}", e)))?;

        Ok(tokens)
    }

    /// Get all tokens for a session (for ACP disconnect handling).
    pub fn get_tokens_by_session(&self, session_id: &str) -> Result<Vec<SystemToken>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, project_root, issued_at, expires_at,
                        heartbeat_interval_secs, heartbeat_at, status
                 FROM system_tokens
                 WHERE session_id = ?1",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare statement: {}", e)))?;

        let tokens = stmt
            .query_map(params![session_id], |row| {
                Ok(SystemToken {
                    id: TokenId::from_string(row.get(0)?),
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    project_root: row.get(3)?,
                    issued_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    heartbeat_interval_secs: row.get::<_, i64>(6)? as u64,
                    heartbeat_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
                        .with_timezone(&Utc),
                    status: match row.get::<_, String>(8)?.as_str() {
                        "ACTIVE" => TokenStatus::Active,
                        "UPGRADING" => TokenStatus::Upgrading,
                        "EXPIRED" => TokenStatus::Expired,
                        "REVOKED" => TokenStatus::Revoked,
                        _ => TokenStatus::Expired,
                    },
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query tokens: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect tokens: {}", e)))?;

        Ok(tokens)
    }

    /// Get all locks held by a token (for reclaim on timeout).
    pub fn get_locks_by_token(&self, token_id: &str) -> Result<Vec<FileLock>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, agent_id, session_id, mode, scope, token_id,
                        reason, approved_by, created_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_locks
                 WHERE token_id = ?1 AND status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare statement: {}", e)))?;

        let locks = stmt
            .query_map(params![token_id], parse_file_lock_row)
            .map_err(|e| ErgataiError::internal(format!("Failed to query locks: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect locks: {}", e)))?;

        Ok(locks)
    }

    /// Get all active locks for a session (for ACP disconnect reclaim).
    ///
    /// Unlike `get_locks_by_token` which queries by file_token_id,
    /// this queries by session_id — which is what the watchdog needs
    /// when reclaiming all locks for a disconnected agent session.
    pub fn get_locks_by_session(&self, session_id: &str) -> Result<Vec<FileLock>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, agent_id, session_id, mode, scope, token_id,
                        reason, approved_by, created_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_locks
                 WHERE session_id = ?1 AND status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare statement: {}", e)))?;

        let locks = stmt
            .query_map(params![session_id], parse_file_lock_row)
            .map_err(|e| ErgataiError::internal(format!("Failed to query locks: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect locks: {}", e)))?;

        Ok(locks)
    }

    /// Mark a token as expired (for watchdog timeout handling).
    pub fn expire_token(&self, token_id: &str) -> Result<(), ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute(
            "UPDATE system_tokens SET status = 'EXPIRED' WHERE id = ?1",
            params![token_id],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to expire token: {}", e)))?;

        info!("Token {} marked as expired", token_id);
        Ok(())
    }

    /// Find an active FileToken by session_id.
    ///
    /// Returns the most recently issued active FileToken for the given session.
    pub fn find_active_file_token_by_session(
        &self,
        session_id: &str,
    ) -> Result<FileToken, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, system_token_id, scope, mode, reason,
                        approved_by, issued_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_tokens
                 WHERE session_id = ?1 AND status = 'ACTIVE'
                 ORDER BY issued_at DESC
                 LIMIT 1",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let token = stmt
            .query_row(params![session_id], |row| {
                Ok(FileToken {
                    id: TokenId::from_string(row.get::<_, String>(0)?),
                    agent_id: row.get::<_, String>(1)?,
                    session_id: row.get::<_, String>(2)?,
                    system_token_id: TokenId::from_string(row.get::<_, String>(3)?),
                    scope: row.get::<_, String>(4)?,
                    mode: parse_file_mode(&row.get::<_, String>(5)?),
                    reason: row.get::<_, Option<String>>(6)?,
                    approved_by: row.get::<_, String>(7)?,
                    issued_at: parse_datetime(&row.get::<_, String>(8)?),
                    expires_at: parse_datetime(&row.get::<_, String>(9)?),
                    heartbeat_interval_secs: row.get::<_, u64>(10)?,
                    heartbeat_at: parse_datetime(&row.get::<_, String>(11)?),
                    status: parse_token_status(&row.get::<_, String>(12)?),
                })
            })
            .map_err(|e| ErgataiError::NotFound(format!("FileToken not found: {}", e)))?;

        Ok(token)
    }

    /// Find an active SystemToken by session_id.
    ///
    /// Returns the most recently issued active SystemToken for the given session.
    pub fn find_active_system_token_by_session(
        &self,
        session_id: &str,
    ) -> Result<SystemToken, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, project_root, issued_at, expires_at,
                        heartbeat_interval_secs, heartbeat_at, status
                 FROM system_tokens
                 WHERE session_id = ?1 AND status = 'ACTIVE'
                 ORDER BY issued_at DESC
                 LIMIT 1",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let token = stmt
            .query_row(params![session_id], |row| {
                Ok(SystemToken {
                    id: TokenId::from_string(row.get::<_, String>(0)?),
                    agent_id: row.get::<_, String>(1)?,
                    session_id: row.get::<_, String>(2)?,
                    project_root: row.get::<_, String>(3)?,
                    issued_at: parse_datetime(&row.get::<_, String>(4)?),
                    expires_at: parse_datetime(&row.get::<_, String>(5)?),
                    heartbeat_interval_secs: row.get::<_, u64>(6)?,
                    heartbeat_at: parse_datetime(&row.get::<_, String>(7)?),
                    status: parse_token_status(&row.get::<_, String>(8)?),
                })
            })
            .map_err(|e| ErgataiError::NotFound(format!("SystemToken not found: {}", e)))?;

        Ok(token)
    }

    /// Register a FileToken in the database.
    pub fn register_file_token(&self, token: &FileToken) -> Result<(), ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute(
            "INSERT INTO file_tokens (
                id, agent_id, session_id, system_token_id, scope, mode, reason,
                approved_by, issued_at, expires_at, heartbeat_interval_secs,
                heartbeat_at, status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                token.id.to_string(),
                token.agent_id,
                token.session_id,
                token.system_token_id.to_string(),
                token.scope,
                format!("{:?}", token.mode),
                token.reason,
                token.approved_by,
                token.issued_at.to_rfc3339(),
                token.expires_at.to_rfc3339(),
                token.heartbeat_interval_secs,
                token.heartbeat_at.to_rfc3339(),
                token.status.to_string(),
            ],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to register file token: {}", e)))?;

        info!("FileToken {} registered for agent {}", token.id, token.agent_id);
        Ok(())
    }

    /// Find an active FileToken by token_id.
    pub fn find_active_file_token_by_id(
        &self,
        token_id: &str,
    ) -> Result<FileToken, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, system_token_id, scope, mode, reason,
                        approved_by, issued_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_tokens
                 WHERE id = ?1 AND status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let token = stmt
            .query_row(params![token_id], |row| {
                Ok(FileToken {
                    id: TokenId::from_string(row.get::<_, String>(0)?),
                    agent_id: row.get::<_, String>(1)?,
                    session_id: row.get::<_, String>(2)?,
                    system_token_id: TokenId::from_string(row.get::<_, String>(3)?),
                    scope: row.get::<_, String>(4)?,
                    mode: parse_file_mode(&row.get::<_, String>(5)?),
                    reason: row.get::<_, Option<String>>(6)?,
                    approved_by: row.get::<_, String>(7)?,
                    issued_at: parse_datetime(&row.get::<_, String>(8)?),
                    expires_at: parse_datetime(&row.get::<_, String>(9)?),
                    heartbeat_interval_secs: row.get::<_, u64>(10)?,
                    heartbeat_at: parse_datetime(&row.get::<_, String>(11)?),
                    status: parse_token_status(&row.get::<_, String>(12)?),
                })
            })
            .map_err(|e| ErgataiError::NotFound(format!("FileToken not found: {}", e)))?;

        Ok(token)
    }

    /// Add a waiter for READ_LATEST (file_path → notification channel).
    ///
    /// Returns a receiver that will be notified when the file is ready or has an error.
    pub async fn add_waiter(
        &self,
        file_path: &str,
    ) -> Result<oneshot::Receiver<Result<(), String>>, ErgataiError> {
        let (tx, rx) = oneshot::channel();
        let mut waiters = self.waiters.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire waiters lock: {}", e))
        })?;
        waiters
            .entry(file_path.to_string())
            .or_insert_with(Vec::new)
            .push(tx);
        debug!("Added waiter for file {}", file_path);
        Ok(rx)
    }

    /// Notify waiters that a file is ready (WRITE completed).
    pub async fn notify_file_ready(&self, file_path: &str) -> Result<(), ErgataiError> {
        let mut waiters = self.waiters.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire waiters lock: {}", e))
        })?;
        if let Some(waiters_list) = waiters.remove(file_path) {
            info!(
                "Notifying {} waiters that file {} is ready",
                waiters_list.len(),
                file_path
            );
            for tx in waiters_list {
                if tx.send(Ok(())).is_err() {
                    warn!("Waiter for file {} already dropped", file_path);
                }
            }
        }
        Ok(())
    }

    /// Notify waiters that a file has an error (writer crashed).
    pub async fn notify_file_error(&self, file_path: &str, reason: &str) -> Result<(), ErgataiError> {
        let mut waiters = self.waiters.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire waiters lock: {}", e))
        })?;
        if let Some(waiters_list) = waiters.remove(file_path) {
            warn!(
                "Notifying {} waiters that file {} has error: {}",
                waiters_list.len(),
                file_path,
                reason
            );
            let error_msg = reason.to_string();
            for tx in waiters_list {
                if tx.send(Err(error_msg.clone())).is_err() {
                    warn!("Waiter for file {} already dropped", file_path);
                }
            }
        }
        Ok(())
    }

    /// Check if a file is locked for writing.
    pub fn is_file_locked_for_write(&self, file_path: &str) -> Result<bool, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let has_lock: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM file_locks
                 WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'",
                params![file_path],
                |row| row.get(0),
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to check lock: {}", e)))?;

        Ok(has_lock)
    }

    /// Read the latest version of a file (READ_LATEST semantics).
    ///
    /// Waits for any pending WRITE to complete or fail before reading.
    /// Returns the file content as bytes.
    pub async fn read_latest(&self, file_path: &str) -> Result<Vec<u8>, ErgataiError> {
        // Check if file is locked for WRITE
        if self.is_file_locked_for_write(file_path)? {
            // Wait for file to become ready
            let rx = self.add_waiter(file_path).await?;

            // Wait for notification or timeout (30 seconds)
            match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                Ok(Ok(Ok(()))) => {
                    // File is ready, proceed to read
                    debug!("File {} is ready, reading", file_path);
                }
                Ok(Ok(Err(reason))) => {
                    // File has error
                    return Err(ErgataiError::internal(format!(
                        "File {} has error: {}",
                        file_path, reason
                    )));
                }
                Ok(Err(_)) => {
                    // Channel closed (sender dropped)
                    return Err(ErgataiError::internal(format!(
                        "Waiter channel closed for file {}",
                        file_path
                    )));
                }
                Err(_) => {
                    // Timeout
                    return Err(ErgataiError::internal(format!(
                        "Timeout waiting for file {} to become ready",
                        file_path
                    )));
                }
            }
        }

        // Read the file
        let full_path = self.project_root.join(file_path);
        tokio::fs::read(&full_path)
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to read file {}: {}", file_path, e)))
    }
}

// Helper functions for parsing database values

fn parse_file_mode(s: &str) -> FileMode {
    match s.to_uppercase().as_str() {
        "READ" => FileMode::Read,
        "WRITE" => FileMode::Write,
        "ADMIN" => FileMode::Admin,
        other => {
            tracing::warn!(mode = other, "Unknown file mode in DB, defaulting to Read (least privilege)");
            FileMode::Read
        }
    }
}

fn parse_token_status(s: &str) -> TokenStatus {
    match s.to_uppercase().as_str() {
        "ACTIVE" => TokenStatus::Active,
        "UPGRADING" => TokenStatus::Upgrading,
        "EXPIRED" => TokenStatus::Expired,
        "REVOKED" => TokenStatus::Revoked,
        other => {
            tracing::warn!(status = other, "Unknown token status in DB, defaulting to Expired (fail-safe)");
            TokenStatus::Expired
        }
    }
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Shared row parser for file_locks queries (14-column SELECT).
/// Used by both `get_locks_by_token` and `get_locks_by_session`.
fn parse_file_lock_row(row: &rusqlite::Row) -> rusqlite::Result<FileLock> {
    Ok(FileLock {
        id: row.get(0)?,
        file_path: row.get(1)?,
        agent_id: row.get(2)?,
        session_id: row.get(3)?,
        mode: match row.get::<_, String>(4)?.as_str() {
            "WRITE" => FileMode::Write,
            _ => FileMode::Read,
        },
        scope: row.get(5)?,
        token_id: TokenId::from_string(row.get(6)?),
        reason: row.get(7)?,
        approved_by: row.get(8)?,
        created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(9)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(10)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        heartbeat_interval_secs: row.get::<_, i64>(11)? as u64,
        heartbeat_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(12)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        status: match row.get::<_, String>(13)?.as_str() {
            "ACTIVE" => TokenStatus::Active,
            "UPGRADING" => TokenStatus::Upgrading,
            "EXPIRED" => TokenStatus::Expired,
            "REVOKED" => TokenStatus::Revoked,
            _ => TokenStatus::Expired,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_manager() -> (FileLockManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let project_root = temp_dir.path().to_path_buf();

        // Create a test file
        std::fs::write(project_root.join("test.txt"), "test content").unwrap();

        let manager = FileLockManager::new(&db_path, project_root).unwrap();
        (manager, temp_dir)
    }

    #[test]
    fn test_wal_mode_enabled() {
        let (manager, _temp) = create_test_manager();
        let conn = manager.conn.lock().unwrap();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
    }

    #[tokio::test]
    async fn test_acquire_and_release_lock() {
        let (manager, _temp) = create_test_manager();

        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );

        manager.register_system_token(&system_token).unwrap();

        let file_token = FileToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Write,
            Some("test".to_string()),
            "system".to_string(),
            3600,
            15,
        );

        // Acquire lock
        manager.acquire_lock(&file_token, "test.txt").unwrap();

        // Check lock exists
        assert!(manager.is_file_locked("test.txt").unwrap());

        // Release lock
        manager.release_lock(file_token.id.as_str(), "test.txt").await.unwrap();

        // Check lock released
        assert!(!manager.is_file_locked("test.txt").unwrap());
    }

    #[test]
    fn test_write_conflict_detection() {
        let (manager, _temp) = create_test_manager();

        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );

        manager.register_system_token(&system_token).unwrap();

        let token1 = FileToken::new(
            "agent-1".to_string(),
            "session-1".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );

        let token2 = FileToken::new(
            "agent-2".to_string(),
            "session-2".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );

        // First agent acquires lock
        manager.acquire_lock(&token1, "test.txt").unwrap();

        // Second agent should fail (conflict)
        let result = manager.acquire_lock(&token2, "test.txt");
        assert!(matches!(result, Err(ErgataiError::LockConflict(_))));
    }

    #[test]
    fn test_path_traversal_rejected() {
        let (manager, _temp) = create_test_manager();

        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );

        manager.register_system_token(&system_token).unwrap();

        let file_token = FileToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );

        // Try path traversal
        let result = manager.acquire_lock(&file_token, "../etc/passwd");
        assert!(matches!(result, Err(ErgataiError::InvalidPath(_))));
    }
}
