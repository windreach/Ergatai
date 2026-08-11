//! Lock upgrade and downgrade for file access control.
//!
//! Allows dynamic switching between READ and WRITE modes without releasing the lock.
//! This is more efficient than releasing and re-acquiring the lock.

use crate::error::ErgataiError;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

use super::token::FileMode;

/// Lock upgrade/downgrade manager
pub struct LockModeManager {
    /// SQLite connection (shared with FileLockManager)
    conn: Arc<Mutex<Connection>>,
}

impl LockModeManager {
    /// Create a new LockModeManager
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Upgrade a READ lock to WRITE
    ///
    /// # Arguments
    /// * `token_id` - Token ID
    /// * `file_path` - File path
    ///
    /// # Returns
    /// Ok(()) if upgrade succeeded, Err if conflict or error
    pub fn upgrade_lock(&self, token_id: &str, file_path: &str) -> Result<(), ErgataiError> {
        info!(
            token_id = %token_id,
            file_path = %file_path,
            "Upgrading lock from READ to WRITE"
        );

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Check if current lock exists and is READ mode
        let current_mode: Option<String> = match conn
            .query_row(
                "SELECT mode FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
                 LIMIT 1",
                params![token_id, file_path],
                |row| row.get(0),
            ) {
                Ok(mode) => Some(mode),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    conn.execute_batch("ROLLBACK").ok();
                    return Err(ErgataiError::internal(format!("Failed to query current lock mode: {}", e)));
                }
            };

        match current_mode {
            Some(mode) if mode == "READ" => {
                // Check for WRITE conflicts
                let conflict = match conn
                    .query_row(
                        "SELECT agent_id FROM file_locks
                         WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'
                         AND token_id != ?2
                         LIMIT 1",
                        params![file_path, token_id],
                        |row| row.get::<_, String>(0),
                    ) {
                        Ok(agent_id) => Some(agent_id),
                        Err(rusqlite::Error::QueryReturnedNoRows) => None,
                        Err(e) => {
                            conn.execute_batch("ROLLBACK").ok();
                            return Err(ErgataiError::internal(format!("Failed to check WRITE conflicts: {}", e)));
                        }
                    };

                if let Some(conflict_agent) = conflict {
                    conn.execute_batch("ROLLBACK").ok();
                    warn!(
                        token_id = %token_id,
                        file_path = %file_path,
                        conflict_agent = %conflict_agent,
                        "Cannot upgrade lock: WRITE conflict detected"
                    );
                    return Err(ErgataiError::LockConflict(format!(
                        "Cannot upgrade lock on {}: already locked for writing by {}",
                        file_path, conflict_agent
                    )));
                }

                // Upgrade to WRITE
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE file_locks SET mode = 'WRITE', updated_at = ?1
                     WHERE token_id = ?2 AND file_path = ?3",
                    params![now, token_id, file_path],
                )
                .map_err(|e| {
                    conn.execute_batch("ROLLBACK").ok();
                    ErgataiError::internal(format!("Failed to upgrade lock: {}", e))
                })?;

                conn.execute_batch("COMMIT")
                    .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

                info!(
                    token_id = %token_id,
                    file_path = %file_path,
                    "Lock upgraded from READ to WRITE"
                );
                Ok(())
            }
            Some(mode) if mode == "WRITE" => {
                conn.execute_batch("ROLLBACK").ok();
                debug!(
                    token_id = %token_id,
                    file_path = %file_path,
                    "Lock already in WRITE mode, no upgrade needed"
                );
                Ok(())
            }
            Some(mode) => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::InvalidArgument(format!(
                    "Cannot upgrade lock in {} mode",
                    mode
                )))
            }
            None => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::NotFound(format!(
                    "No active lock found for token {} on file {}",
                    token_id, file_path
                )))
            }
        }
    }

    /// Downgrade a WRITE lock to READ
    ///
    /// # Arguments
    /// * `token_id` - Token ID
    /// * `file_path` - File path
    ///
    /// # Returns
    /// Ok(()) if downgrade succeeded, Err if error
    pub fn downgrade_lock(&self, token_id: &str, file_path: &str) -> Result<(), ErgataiError> {
        info!(
            token_id = %token_id,
            file_path = %file_path,
            "Downgrading lock from WRITE to READ"
        );

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Check if current lock exists and is WRITE mode
        let current_mode: Option<String> = match conn
            .query_row(
                "SELECT mode FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
                 LIMIT 1",
                params![token_id, file_path],
                |row| row.get(0),
            ) {
                Ok(mode) => Some(mode),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    conn.execute_batch("ROLLBACK").ok();
                    return Err(ErgataiError::internal(format!("Failed to query current lock mode: {}", e)));
                }
            };

        match current_mode {
            Some(mode) if mode == "WRITE" => {
                // Downgrade to READ
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE file_locks SET mode = 'READ', updated_at = ?1
                     WHERE token_id = ?2 AND file_path = ?3",
                    params![now, token_id, file_path],
                )
                .map_err(|e| {
                    conn.execute_batch("ROLLBACK").ok();
                    ErgataiError::internal(format!("Failed to downgrade lock: {}", e))
                })?;

                conn.execute_batch("COMMIT")
                    .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

                info!(
                    token_id = %token_id,
                    file_path = %file_path,
                    "Lock downgraded from WRITE to READ"
                );
                Ok(())
            }
            Some(mode) if mode == "READ" => {
                conn.execute_batch("ROLLBACK").ok();
                debug!(
                    token_id = %token_id,
                    file_path = %file_path,
                    "Lock already in READ mode, no downgrade needed"
                );
                Ok(())
            }
            Some(mode) => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::InvalidArgument(format!(
                    "Cannot downgrade lock in {} mode",
                    mode
                )))
            }
            None => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::NotFound(format!(
                    "No active lock found for token {} on file {}",
                    token_id, file_path
                )))
            }
        }
    }

    /// Get current lock mode for a file
    pub fn get_lock_mode(
        &self,
        token_id: &str,
        file_path: &str,
    ) -> Result<Option<FileMode>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mode: Option<String> = match conn
            .query_row(
                "SELECT mode FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
                 LIMIT 1",
                params![token_id, file_path],
                |row| row.get(0),
            ) {
                Ok(m) => Some(m),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    return Err(ErgataiError::internal(format!("Failed to query lock mode: {}", e)));
                }
            };

        match mode.as_deref() {
            Some("READ") => Ok(Some(FileMode::Read)),
            Some("WRITE") => Ok(Some(FileMode::Write)),
            Some("ADMIN") => Ok(Some(FileMode::Admin)),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, LockModeManager) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_locks.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS file_locks (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                mode TEXT NOT NULL,
                token_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                updated_at TEXT
            );
            ",
        )
        .unwrap();

        let conn = Arc::new(Mutex::new(conn));
        let manager = LockModeManager::new(conn);

        (temp_dir, manager)
    }

    #[test]
    fn test_upgrade_read_to_write() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a READ lock
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, agent_id, session_id, mode, token_id, status, created_at, expires_at)
                 VALUES ('lock1', 'test.rs', 'agent1', 'session1', 'READ', 'token1', 'ACTIVE', datetime('now'), datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Upgrade to WRITE
        let result = manager.upgrade_lock("token1", "test.rs");
        assert!(result.is_ok());

        // Verify mode changed
        let mode = manager.get_lock_mode("token1", "test.rs").unwrap();
        assert_eq!(mode, Some(FileMode::Write));
    }

    #[test]
    fn test_downgrade_write_to_read() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a WRITE lock
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, agent_id, session_id, mode, token_id, status, created_at, expires_at)
                 VALUES ('lock1', 'test.rs', 'agent1', 'session1', 'WRITE', 'token1', 'ACTIVE', datetime('now'), datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Downgrade to READ
        let result = manager.downgrade_lock("token1", "test.rs");
        assert!(result.is_ok());

        // Verify mode changed
        let mode = manager.get_lock_mode("token1", "test.rs").unwrap();
        assert_eq!(mode, Some(FileMode::Read));
    }

    #[test]
    fn test_upgrade_conflict() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a READ lock for agent1
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, agent_id, session_id, mode, token_id, status, created_at, expires_at)
                 VALUES ('lock1', 'test.rs', 'agent1', 'session1', 'READ', 'token1', 'ACTIVE', datetime('now'), datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();

            // Insert a WRITE lock for agent2 (conflict)
            conn.execute(
                "INSERT INTO file_locks (id, file_path, agent_id, session_id, mode, token_id, status, created_at, expires_at)
                 VALUES ('lock2', 'test.rs', 'agent2', 'session2', 'WRITE', 'token2', 'ACTIVE', datetime('now'), datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Try to upgrade agent1's lock - should fail due to conflict
        let result = manager.upgrade_lock("token1", "test.rs");
        assert!(result.is_err());
    }
}
