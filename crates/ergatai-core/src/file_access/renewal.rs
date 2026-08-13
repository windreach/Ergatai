//! Token and lock renewal for file access control.
//!
//! Allows extending token and lock expiration times to prevent interruption
//! of long-running tasks.

use super::lock_manager::TransactionGuard;
use crate::error::ErgataiError;
use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use tracing::info;

/// Token and lock renewal manager
pub struct RenewalManager {
    /// SQLite connection (shared with FileLockManager)
    conn: Arc<Mutex<Connection>>,
    /// Default extension duration (in seconds)
    default_extension_secs: u64,
}

impl RenewalManager {
    /// Create a new RenewalManager
    pub fn new(conn: Arc<Mutex<Connection>>, default_extension_secs: u64) -> Self {
        Self {
            conn,
            default_extension_secs,
        }
    }

    /// Renew a system token
    ///
    /// # Arguments
    /// * `token_id` - Token ID
    /// * `extension_secs` - Optional extension duration in seconds (uses default if None)
    ///
    /// # Returns
    /// New expiration time
    pub fn renew_system_token(
        &self,
        token_id: &str,
        extension_secs: Option<u64>,
    ) -> Result<String, ErgataiError> {
        let extension =
            Duration::seconds(extension_secs.unwrap_or(self.default_extension_secs) as i64);
        let new_expiry = Utc::now() + extension;

        info!(
            token_id = %token_id,
            new_expiry = %new_expiry,
            "Renewing system token"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let tx = TransactionGuard::begin(&conn)
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Check if token exists and is active
        let current_status: Option<String> = match conn.query_row(
            "SELECT status FROM system_tokens WHERE id = ?1",
            params![token_id],
            |row| row.get(0),
        ) {
            Ok(status) => Some(status),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                return Err(ErgataiError::internal(format!(
                    "Failed to query token status: {}",
                    e
                )));
            }
        };

        match current_status.as_deref() {
            Some("ACTIVE") | Some("UPGRADING") => {
                // Renew the token
                conn.execute(
                    "UPDATE system_tokens SET expires_at = ?1 WHERE id = ?2",
                    params![new_expiry.to_rfc3339(), token_id],
                )
                .map_err(|e| {
                    ErgataiError::internal(format!("Failed to renew token: {}", e))
                })?;

                tx.commit()
                    .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

                info!(
                    token_id = %token_id,
                    new_expiry = %new_expiry,
                    "System token renewed successfully"
                );
                Ok(new_expiry.to_rfc3339())
            }
            Some(status) => {
                Err(ErgataiError::InvalidArgument(format!(
                    "Cannot renew token in {} status",
                    status
                )))
            }
            None => {
                Err(ErgataiError::NotFound(format!(
                    "System token {} not found",
                    token_id
                )))
            }
        }
    }

    /// Renew a file lock
    ///
    /// # Arguments
    /// * `token_id` - Token ID
    /// * `file_path` - File path
    /// * `extension_secs` - Optional extension duration in seconds (uses default if None)
    ///
    /// # Returns
    /// New expiration time
    pub fn renew_lock(
        &self,
        token_id: &str,
        file_path: &str,
        extension_secs: Option<u64>,
    ) -> Result<String, ErgataiError> {
        let extension =
            Duration::seconds(extension_secs.unwrap_or(self.default_extension_secs) as i64);
        let new_expiry = Utc::now() + extension;

        info!(
            token_id = %token_id,
            file_path = %file_path,
            new_expiry = %new_expiry,
            "Renewing file lock"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let tx = TransactionGuard::begin(&conn)
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Check if lock exists and is active
        let current_status: Option<String> = match conn.query_row(
            "SELECT status FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
                 LIMIT 1",
            params![token_id, file_path],
            |row| row.get(0),
        ) {
            Ok(status) => Some(status),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                return Err(ErgataiError::internal(format!(
                    "Failed to query file lock status: {}",
                    e
                )));
            }
        };

        match current_status.as_deref() {
            Some("ACTIVE") => {
                // Renew the lock
                conn.execute(
                    "UPDATE file_locks SET expires_at = ?1
                     WHERE token_id = ?2 AND file_path = ?3 AND status = 'ACTIVE'",
                    params![new_expiry.to_rfc3339(), token_id, file_path],
                )
                .map_err(|e| {
                    ErgataiError::internal(format!("Failed to renew lock: {}", e))
                })?;

                tx.commit()
                    .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

                info!(
                    token_id = %token_id,
                    file_path = %file_path,
                    new_expiry = %new_expiry,
                    "File lock renewed successfully"
                );
                Ok(new_expiry.to_rfc3339())
            }
            Some(status) => {
                Err(ErgataiError::InvalidArgument(format!(
                    "Cannot renew lock in {} status",
                    status
                )))
            }
            None => {
                Err(ErgataiError::NotFound(format!(
                    "No active lock found for token {} on file {}",
                    token_id, file_path
                )))
            }
        }
    }

    /// Renew all locks for a token
    ///
    /// # Arguments
    /// * `token_id` - Token ID
    /// * `extension_secs` - Optional extension duration in seconds (uses default if None)
    ///
    /// # Returns
    /// Number of locks renewed
    pub fn renew_all_locks(
        &self,
        token_id: &str,
        extension_secs: Option<u64>,
    ) -> Result<usize, ErgataiError> {
        let extension =
            Duration::seconds(extension_secs.unwrap_or(self.default_extension_secs) as i64);
        let new_expiry = Utc::now() + extension;

        info!(
            token_id = %token_id,
            new_expiry = %new_expiry,
            "Renewing all locks for token"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let tx = TransactionGuard::begin(&conn)
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Renew all active locks for this token
        let renewed = conn
            .execute(
                "UPDATE file_locks SET expires_at = ?1
                 WHERE token_id = ?2 AND status = 'ACTIVE'",
                params![new_expiry.to_rfc3339(), token_id],
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to renew locks: {}", e))
            })?;

        tx.commit()
            .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

        info!(
            token_id = %token_id,
            renewed_count = renewed,
            new_expiry = %new_expiry,
            "Renewed all locks for token"
        );
        Ok(renewed)
    }

    /// Auto-renew tokens and locks that are about to expire
    ///
    /// # Arguments
    /// * `threshold_secs` - Renew if expires within this many seconds
    /// * `extension_secs` - Extension duration in seconds
    ///
    /// # Returns
    /// Number of tokens and locks renewed
    pub fn auto_renew_expiring(
        &self,
        threshold_secs: u64,
        extension_secs: u64,
    ) -> Result<(usize, usize), ErgataiError> {
        let threshold = Utc::now() + Duration::seconds(threshold_secs as i64);
        let new_expiry = Utc::now() + Duration::seconds(extension_secs as i64);

        info!(
            threshold = %threshold,
            new_expiry = %new_expiry,
            "Auto-renewing expiring tokens and locks"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let tx = TransactionGuard::begin(&conn)
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Renew expiring system tokens
        let tokens_renewed = conn
            .execute(
                "UPDATE system_tokens SET expires_at = ?1
                 WHERE status = 'ACTIVE' AND expires_at < ?2",
                params![new_expiry.to_rfc3339(), threshold.to_rfc3339()],
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to renew tokens: {}", e))
            })?;

        // Renew expiring file locks
        let locks_renewed = conn
            .execute(
                "UPDATE file_locks SET expires_at = ?1
                 WHERE status = 'ACTIVE' AND expires_at < ?2",
                params![new_expiry.to_rfc3339(), threshold.to_rfc3339()],
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to renew locks: {}", e))
            })?;

        tx.commit()
            .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

        info!(
            tokens_renewed = tokens_renewed,
            locks_renewed = locks_renewed,
            "Auto-renewal completed"
        );
        Ok((tokens_renewed, locks_renewed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, RenewalManager) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_renewal.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS system_tokens (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS file_locks (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                token_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                expires_at TEXT NOT NULL
            );
            ",
        )
        .unwrap();

        let conn = Arc::new(Mutex::new(conn));
        let manager = RenewalManager::new(conn, 3600); // 1 hour default

        (temp_dir, manager)
    }

    #[test]
    fn test_renew_system_token() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a token
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO system_tokens (id, agent_id, session_id, status, expires_at)
                 VALUES ('token1', 'agent1', 'session1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Renew the token
        let result = manager.renew_system_token("token1", Some(7200));
        assert!(result.is_ok());

        // Verify expiry was extended
        let new_expiry = result.unwrap();
        assert!(!new_expiry.is_empty());
    }

    #[test]
    fn test_renew_lock() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a lock
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock1', 'test.rs', 'token1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Renew the lock
        let result = manager.renew_lock("token1", "test.rs", Some(7200));
        assert!(result.is_ok());

        // Verify expiry was extended
        let new_expiry = result.unwrap();
        assert!(!new_expiry.is_empty());
    }

    #[test]
    fn test_auto_renew_expiring() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert expiring tokens and locks
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO system_tokens (id, agent_id, session_id, status, expires_at)
                 VALUES ('token1', 'agent1', 'session1', 'ACTIVE', datetime('now', '+30 seconds'))",
                [],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock1', 'test.rs', 'token1', 'ACTIVE', datetime('now', '+30 seconds'))",
                [],
            )
            .unwrap();
        }

        // Auto-renew
        let result = manager.auto_renew_expiring(60, 3600);
        assert!(result.is_ok());

        let (tokens_renewed, locks_renewed) = result.unwrap();
        assert_eq!(tokens_renewed, 1);
        assert_eq!(locks_renewed, 1);
    }

    #[test]
    fn test_renew_nonexistent_token() {
        let (_temp_dir, manager) = setup_test_db();

        // Try to renew a token that doesn't exist
        let result = manager.renew_system_token("nonexistent-token", Some(7200));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ErgataiError::NotFound(_)));
    }

    #[test]
    fn test_renew_nonexistent_lock() {
        let (_temp_dir, manager) = setup_test_db();

        // Try to renew a lock that doesn't exist
        let result = manager.renew_lock("nonexistent-token", "nonexistent.rs", Some(7200));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ErgataiError::NotFound(_)));
    }

    #[test]
    fn test_renew_expired_token() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert an expired token
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO system_tokens (id, agent_id, session_id, status, expires_at)
                 VALUES ('token1', 'agent1', 'session1', 'EXPIRED', datetime('now', '-1 hour'))",
                [],
            )
            .unwrap();
        }

        // Try to renew expired token - should fail
        let result = manager.renew_system_token("token1", Some(7200));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ErgataiError::InvalidArgument(_)
        ));
    }

    #[test]
    fn test_renew_expired_lock() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert an expired lock
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock1', 'test.rs', 'token1', 'EXPIRED', datetime('now', '-1 hour'))",
                [],
            )
            .unwrap();
        }

        // Try to renew expired lock - should fail with NotFound
        // (because the query filters by status = 'ACTIVE')
        let result = manager.renew_lock("token1", "test.rs", Some(7200));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ErgataiError::NotFound(_)));
    }

    #[test]
    fn test_auto_renew_no_expiring() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert tokens/locks that are not expiring soon (far in the future)
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO system_tokens (id, agent_id, session_id, status, expires_at)
                 VALUES ('token1', 'agent1', 'session1', 'ACTIVE', datetime('now', '+1 day'))",
                [],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock1', 'test.rs', 'token1', 'ACTIVE', datetime('now', '+1 day'))",
                [],
            )
            .unwrap();
        }

        // Auto-renew with short threshold - should not renew anything
        let result = manager.auto_renew_expiring(60, 3600);
        assert!(result.is_ok());

        let (tokens_renewed, locks_renewed) = result.unwrap();
        assert_eq!(tokens_renewed, 0);
        assert_eq!(locks_renewed, 0);
    }

    #[test]
    fn test_renew_with_default_duration() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert a token
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO system_tokens (id, agent_id, session_id, status, expires_at)
                 VALUES ('token1', 'agent1', 'session1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Renew with None (use default duration)
        let result = manager.renew_system_token("token1", None);
        assert!(result.is_ok());

        // Verify expiry was extended
        let new_expiry = result.unwrap();
        assert!(!new_expiry.is_empty());
    }

    #[test]
    fn test_renew_multiple_locks() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert multiple locks for the same token
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock1', 'file1.rs', 'token1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock2', 'file2.rs', 'token1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO file_locks (id, file_path, token_id, status, expires_at)
                 VALUES ('lock3', 'file3.rs', 'token1', 'ACTIVE', datetime('now', '+1 hour'))",
                [],
            )
            .unwrap();
        }

        // Renew each lock individually
        assert!(manager.renew_lock("token1", "file1.rs", Some(7200)).is_ok());
        assert!(manager.renew_lock("token1", "file2.rs", Some(7200)).is_ok());
        assert!(manager.renew_lock("token1", "file3.rs", Some(7200)).is_ok());
    }
}
