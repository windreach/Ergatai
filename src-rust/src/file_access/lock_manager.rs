//! SQLite-based file lock manager.
//!
//! Uses BEGIN IMMEDIATE + unique index constraints for atomicity.
//! WAL mode enabled for concurrent read performance.

use crate::error::ErgataiError;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tracing::{debug, info, warn, error};

use super::audit::AuditManager;
use super::token::{FileLock, FileMode, FileToken, SystemToken, TokenId, TokenStatus};
use crate::nats::events::{FileAccessEscalatePayload, FileAccessApprovePayload, FileAccessRejectPayload};

/// Stabilization window for single-agent mode detection.
///
/// The system must observe exactly one active session for this entire duration
/// before auto-bypassing the approval flow. Prevents rapid toggling when agents
/// connect/disconnect in quick succession.
const SINGLE_AGENT_STABILIZE_SECS: u64 = 5;

/// How long a disconnected session is considered "temporarily disconnected".
///
/// When a session disconnects, it is marked as temporarily_disconnected for
/// this duration. If it reconnects within this window, the single-agent mode
/// detection does not treat the disconnection as a real departure. This prevents
/// the main agent's temporary disconnect from triggering single-agent bypass.
const SESSION_STICKINESS_SECS: u64 = 30;

/// Approval response from main agent via NATS (for WRITE conflict escalation)
/// M5 fix: Renamed from `WriteConflictApproval` to distinguish from `acp::WriteConflictApproval`
/// which handles ACP permission flow (human approval). This struct is specifically
/// for the NATS-based WRITE conflict arbitration flow.
#[derive(Debug, Clone)]
pub struct WriteConflictApproval {
    pub approved: bool,
    pub approved_by: String,
    pub reason: Option<String>,
}

/// File lock manager backed by SQLite.
///
/// Thread-safe via internal Mutex. All operations use BEGIN IMMEDIATE for atomicity.
///
/// # SAFETY: std::sync::Mutex in async context (M13)
/// This struct uses `std::sync::Mutex` (not `tokio::sync::Mutex`) for `conn`,
/// `waiters`, `retry_tracker`, etc. This is intentional because:
/// - All critical sections are short and contain NO `.await` points
/// - `std::sync::Mutex` has lower overhead than `tokio::sync::Mutex`
/// - No guard is ever held across an `.await` boundary
///
/// **INVARIANT**: Never extend a MutexGuard to include an `.await`. If you need
/// to call an async function while holding data from a guard, clone the data first,
/// drop the guard, then await.
// L1 fix: type aliases for complex nested types
type FileWaiters = HashMap<String, Vec<oneshot::Sender<Result<(), String>>>>;
type RetryTracker = HashMap<(String, String), u32>;

pub struct FileLockManager {
    /// SQLite connection (wrapped in Mutex for thread safety).
    conn: Arc<Mutex<Connection>>,
    /// Project root directory (for path canonicalization).
    project_root: PathBuf,
    /// Cached canonical project root (M2 fix: avoid repeated I/O).
    project_root_canonical: PathBuf,
    /// Waiters for READ_LATEST (file_path → list of notification channels).
    waiters: Arc<Mutex<FileWaiters>>,
    /// Number of currently active ACP sessions (sessions holding system tokens).
    ///
    /// Updated by `register_session` / `unregister_session` from the ACP session
    /// lifecycle. Read lock-free via `Ordering::Relaxed`.
    active_session_count: Arc<AtomicUsize>,
    /// Instant at which the session count first became exactly 1.
    ///
    /// Reset to `None` whenever the count changes. `is_single_agent_mode()` returns
    /// true only when this has been `Some` for at least `SINGLE_AGENT_STABILIZE_SECS`.
    single_agent_since: Arc<Mutex<Option<Instant>>>,
    /// Per-(file_path, agent_id) retry counter for livelock prevention.
    ///
    /// Tracks how many times an agent has been rejected from acquiring a WRITE lock
    /// on a specific file. Used for:
    /// - Computing exponential backoff duration
    /// - Priority boost during arbitration (waiting agents get higher effective priority)
    /// - Giving up after MAX_RETRIES (5) to prevent infinite loops
    retry_tracker: Arc<Mutex<RetryTracker>>,

    // ===== NATS Approval Integration =====
    /// NATS client for approval flow communication (optional, None = degraded mode)
    nats_client: Option<Arc<async_nats::Client>>,
    /// Background task for processing NATS approval responses
    subscription_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Pending approval requests (request_id → responder channel)
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<WriteConflictApproval>>>>,

    /// Recently disconnected sessions (session_id → disconnect time).
    ///
    /// When a session unregisters, it is recorded here for `SESSION_STICKINESS_SECS`.
    /// If it reconnects within that window, the disconnection is treated as transient
    /// and does not affect single-agent mode detection. This prevents the main agent's
    /// temporary disconnect from triggering the single-agent approval bypass.
    disconnected_sessions: Arc<Mutex<HashMap<String, Instant>>>,

    /// Idempotency cache for approval requests (idempotency_key → (request_id, timestamp)).
    ///
    /// Key: `"{agent_id}:{file_path}:{mode}"` — prevents duplicate NATS messages
    /// when the same agent retries the same file access request within the timeout window.
    pending_request_keys: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}

impl FileLockManager {
    /// Create a new lock manager with the given database path.
    ///
    /// Enables WAL mode and creates tables if they don't exist.
    /// Optionally accepts a NATS client for multi-agent approval flow.
    pub fn new(
        db_path: &Path,
        project_root: PathBuf,
        nats_client: Option<Arc<async_nats::Client>>,
    ) -> Result<Self, ErgataiError> {
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

        // Verify WAL mode was actually enabled (PRAGMA may silently fail on some filesystems)
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|e| ErgataiError::internal(format!("Failed to query journal mode: {}", e)))?;
        if journal_mode.to_lowercase() != "wal" {
            return Err(ErgataiError::internal(format!(
                "Failed to enable WAL journal mode (current: {}). \
                 Concurrent read/write performance may be degraded.",
                journal_mode
            )));
        }

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
            active_session_count: Arc::new(AtomicUsize::new(0)),
            single_agent_since: Arc::new(Mutex::new(None)),
            retry_tracker: Arc::new(Mutex::new(HashMap::new())),
            nats_client,
            subscription_task: Arc::new(Mutex::new(None)),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
            disconnected_sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_request_keys: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Get the project root directory this manager was initialized with.
    pub fn project_root(&self) -> &Path {
        &self.project_root
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
                status TEXT NOT NULL,
                updated_at TEXT,
                priority INTEGER
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

            -- File tokens (per-operation tokens linked to a system token)
            CREATE TABLE IF NOT EXISTS file_tokens (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                system_token_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                mode TEXT NOT NULL,
                reason TEXT,
                approved_by TEXT,
                issued_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                heartbeat_interval_secs INTEGER NOT NULL,
                heartbeat_at TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER,
                FOREIGN KEY (system_token_id) REFERENCES system_tokens(id)
            );

            CREATE INDEX IF NOT EXISTS idx_file_tokens_session
                ON file_tokens(session_id, status);
            CREATE INDEX IF NOT EXISTS idx_file_tokens_agent
                ON file_tokens(agent_id, status);
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
    pub async fn acquire_lock(&self, token: &FileToken, file_path: &str) -> Result<(), ErgataiError> {
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
        // L3 fix: collapsed nested if
        if crate::file_access::sensitive_paths::is_sensitive_path(&normalized_path)
            && token.mode != FileMode::Admin
        {
            return Err(ErgataiError::PermissionDenied(format!(
                "File {} is a sensitive path and requires ADMIN permission (current mode: {:?})",
                file_path, token.mode
            )));
        }

        // Check for WRITE conflict and get conflict info if any
        // This block ensures conn is released before any async operations
        let conflict_info = {
            let conn = self.conn.lock().map_err(|e| {
                ErgataiError::internal(format!("Failed to acquire lock: {}", e))
            })?;

            conn.execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

            // Check for WRITE conflict (unique index will enforce this)
            // In single-agent mode, skip the conflict check — there is no contention risk
            // from other agents, so the approval flow / arbitration is unnecessary overhead.
            let single_agent = self.is_single_agent_mode();
            if token.mode == FileMode::Write && !single_agent {
                // Get conflict information for arbitration
                match conn
                    .query_row(
                        "SELECT agent_id, session_id, token_id, reason, priority FROM file_locks
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
                                    priority: row.get(4)?,
                                    reason: row.get(3)?,
                                },
                                new_requester: crate::file_access::conflict_arbitration::LockHolderInfo {
                                    agent_id: token.agent_id.clone(),
                                    session_id: token.session_id.clone(),
                                    token_id: token.id.as_str().to_string(),
                                    priority: token.priority,
                                    reason: token.reason.clone(),
                                },
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            })
                        },
                    ) {
                        Ok(info) => {
                            // Rollback before dropping conn
                            conn.execute_batch("ROLLBACK").ok();
                            Some(info)
                        }
                        Err(rusqlite::Error::QueryReturnedNoRows) => {
                            // No conflict, rollback
                            conn.execute_batch("ROLLBACK").ok();
                            None
                        }
                        Err(e) => {
                            conn.execute_batch("ROLLBACK").ok();
                            return Err(ErgataiError::internal(format!(
                                "Failed to check WRITE conflict (DB error): {}", e
                            )));
                        }
                    }
                } else {
                    // Not a WRITE lock or single-agent mode, no conflict check needed
                    conn.execute_batch("ROLLBACK").ok();
                    None
                }
        }; // conn is released here

        // If there's a conflict, handle escalation or local arbitration
        if let Some(conflict) = conflict_info {
            // Check if we should escalate to main agent via NATS
            if self.should_escalate_to_main_agent() {
                info!(
                    file_path = %file_path,
                    requester = %token.agent_id,
                    holder = %conflict.current_holder.agent_id,
                    "Escalating WRITE conflict to main agent via NATS"
                );

                // Request approval from main agent
                let request_id = self.request_approval_from_main_agent(
                    token,
                    file_path,
                    Some(&conflict.current_holder.agent_id),
                    &format!("WRITE conflict on {} with agent {}", file_path, conflict.current_holder.agent_id),
                ).await?;

                // Wait for approval response (30 second timeout)
                match self.wait_for_approval(&request_id, Duration::from_secs(30)).await {
                    Ok(response) if response.approved => {
                        info!(
                            request_id = %request_id,
                            approved_by = %response.approved_by,
                            "Main agent approved WRITE access"
                        );
                        // Acquire the lock without conflict check (approved by main agent)
                        return self.acquire_lock_approved(token, file_path).await;
                    }
                    Ok(response) => {
                        warn!(
                            request_id = %request_id,
                            approved_by = %response.approved_by,
                            reason = ?response.reason,
                            "Main agent denied WRITE access"
                        );
                        return Err(ErgataiError::PermissionDenied(format!(
                            "WRITE access denied by main agent {}: {}",
                            response.approved_by,
                            response.reason.unwrap_or_else(|| "No reason provided".to_string())
                        )));
                    }
                    Err(e) => {
                        warn!(
                            request_id = %request_id,
                            error = %e,
                            "NATS approval failed, falling back to local arbitration"
                        );
                        // Fall through to local arbitration
                    }
                }
            }

            // Local arbitration (no NATS escalation or NATS failed)
            match self.perform_local_arbitration(token, file_path, &conflict) {
                Ok(true) => {
                    // Arbitration granted, continue with lock acquisition
                }
                Ok(false) => {
                    // Should not happen
                    return Err(ErgataiError::internal("Arbitration returned false"));
                }
                Err(e) => {
                    // Arbitration rejected
                    return Err(e);
                }
            }
        }

        // No conflict, proceed with lock acquisition
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Clear any stale retry count for this (file, agent)
        if let Ok(mut tracker) = self.retry_tracker.lock() {
            tracker.remove(&(normalized_path.clone(), token.agent_id.clone()));
        }

        // Insert lock record
        let now = Utc::now();
        conn.execute(
            "INSERT INTO file_locks (
                id, file_path, agent_id, session_id, mode, scope, token_id,
                reason, approved_by, created_at, expires_at,
                heartbeat_interval_secs, heartbeat_at, status, priority
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                token.priority.map(|p| p as i64),
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

    /// Acquire lock after main agent approval (skips conflict check).
    ///
    /// This is called when a WRITE conflict has been escalated to and approved by the main agent.
    /// It bypasses the normal conflict detection since we already have approval.
    async fn acquire_lock_approved(&self, token: &FileToken, file_path: &str) -> Result<(), ErgataiError> {
        let normalized_path = self.validate_and_normalize_path(file_path)?;

        if !token.matches_path(&normalized_path) {
            return Err(ErgataiError::PermissionDenied(format!(
                "File {} is outside token scope {}",
                file_path, token.scope
            )));
        }

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Expire any existing WRITE lock (we have approval to preempt)
        if token.mode == FileMode::Write {
            conn.execute(
                "UPDATE file_locks SET status = 'EXPIRED'
                 WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'",
                params![normalized_path],
            ).map_err(|e| {
                conn.execute_batch("ROLLBACK").ok();
                ErgataiError::internal(format!("Failed to expire existing WRITE lock on {}: {}", normalized_path, e))
            })?;
        }

        // Insert lock record
        let now = Utc::now();
        conn.execute(
            "INSERT INTO file_locks (
                id, file_path, agent_id, session_id, mode, scope, token_id,
                reason, approved_by, created_at, expires_at,
                heartbeat_interval_secs, heartbeat_at, status, priority
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                token.priority.map(|p| p as i64),
            ],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            ErgataiError::internal(format!("Failed to insert lock: {}", e))
        })?;

        // Audit log
        let now_audit = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![now_audit, token.agent_id, token.session_id, "LOCK_ACQUIRED_APPROVED", normalized_path, token.mode.to_string(), token.reason],
        )
        .map_err(|e| {
            conn.execute_batch("ROLLBACK").ok();
            ErgataiError::internal(format!("Failed to log audit: {}", e))
        })?;

        conn.execute_batch("COMMIT")
            .map_err(|e| ErgataiError::internal(format!("Failed to commit: {}", e)))?;

        info!(
            "Agent {} acquired {:?} lock on {} (main agent approved)",
            token.agent_id, token.mode, file_path
        );
        Ok(())
    }

    /// Perform local arbitration for a WRITE conflict.
    ///
    /// This is called when NATS escalation is not available or fails.
    /// Uses retry counts and priority boosting to make a decision.
    /// Returns Ok(true) if arbitration granted the lock, Ok(false) if rejected.
    fn perform_local_arbitration(
        &self,
        token: &FileToken,
        file_path: &str,
        conflict: &crate::file_access::conflict_arbitration::ConflictInfo,
    ) -> Result<bool, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        let normalized_path = self.validate_and_normalize_path(file_path)?;

        // Look up retry counts for both agents
        let key_new = (normalized_path.clone(), token.agent_id.clone());
        let new_retry_count = {
            let tracker = self.retry_tracker.lock().map_err(|e| {
                ErgataiError::internal(format!("retry_tracker poisoned: {}", e))
            })?;
            tracker.get(&key_new).copied().unwrap_or(0)
        };

        // Check if new requester has exceeded max retries
        if new_retry_count >= crate::file_access::conflict_arbitration::MAX_RETRIES {
            conn.execute_batch("ROLLBACK").ok();
            if let Ok(mut tracker) = self.retry_tracker.lock() {
                tracker.remove(&key_new);
            }
            return Err(ErgataiError::LockConflict(format!(
                "File {} lock conflict: agent {} exceeded max retries ({})",
                file_path, token.agent_id,
                crate::file_access::conflict_arbitration::MAX_RETRIES
            )));
        }

        // Look up current holder's retry count (for priority boost)
        let curr_retry_count = {
            let key_curr = (normalized_path.clone(), conflict.current_holder.agent_id.clone());
            let tracker = self.retry_tracker.lock().map_err(|e| {
                ErgataiError::internal(format!("retry_tracker poisoned: {}", e))
            })?;
            tracker.get(&key_curr).copied().unwrap_or(0)
        };

        // Arbitrate with priority boost
        let decision = crate::file_access::conflict_arbitration::arbitrate_with_boost(
            conflict, curr_retry_count, new_retry_count,
        );

        match decision {
            crate::file_access::conflict_arbitration::ArbitrationDecision::KeepWithCurrentHolder => {
                conn.execute_batch("ROLLBACK").ok();

                let new_count = {
                    let mut tracker = self.retry_tracker.lock().map_err(|e| {
                        ErgataiError::internal(format!("retry_tracker poisoned: {}", e))
                    })?;
                    let count = tracker.entry(key_new).or_insert(0);
                    *count += 1;
                    *count
                };

                let retry_after_ms = crate::file_access::conflict_arbitration::compute_backoff_ms(new_count);
                let boosted = new_count >= crate::file_access::conflict_arbitration::PRIORITY_BOOST_THRESHOLD;

                tracing::info!(
                    file_path = %file_path,
                    holder = %conflict.current_holder.agent_id,
                    requester = %token.agent_id,
                    retry_count = new_count,
                    retry_after_ms = retry_after_ms,
                    priority_boosted = boosted,
                    "WRITE lock conflict: keep with current holder, requester should back off"
                );

                Err(ErgataiError::LockConflictWithRetry {
                    file_path: file_path.to_string(),
                    message: format!(
                        "File {} already locked for writing by {}",
                        file_path, conflict.current_holder.agent_id
                    ),
                    retry_after_ms,
                    retry_count: new_count,
                    max_retries: crate::file_access::conflict_arbitration::MAX_RETRIES,
                    priority_boosted: boosted,
                })
            }
            crate::file_access::conflict_arbitration::ArbitrationDecision::GrantToNewRequester => {
                conn.execute(
                    "UPDATE file_locks SET status = 'EXPIRED'
                     WHERE file_path = ?1 AND mode = 'WRITE' AND status = 'ACTIVE'",
                    params![normalized_path],
                ).map_err(|e| {
                    conn.execute_batch("ROLLBACK").ok();
                    ErgataiError::internal(format!("Failed to expire WRITE lock during arbitration on {}: {}", normalized_path, e))
                })?;

                if let Ok(mut tracker) = self.retry_tracker.lock() {
                    tracker.remove(&key_new);
                }

                tracing::info!(
                    file_path = %normalized_path,
                    preempted_agent = %conflict.current_holder.agent_id,
                    new_agent = %conflict.new_requester.agent_id,
                    "WRITE lock preempted via arbitration"
                );

                // Rollback since we'll re-acquire in the caller
                conn.execute_batch("ROLLBACK").ok();
                Ok(true) // Continue with lock acquisition
            }
            crate::file_access::conflict_arbitration::ArbitrationDecision::RejectBoth => {
                conn.execute_batch("ROLLBACK").ok();

                let new_count = {
                    let mut tracker = self.retry_tracker.lock().map_err(|e| {
                        ErgataiError::internal(format!("retry_tracker poisoned: {}", e))
                    })?;
                    let count = tracker.entry(key_new).or_insert(0);
                    *count += 1;
                    *count
                };

                let retry_after_ms = crate::file_access::conflict_arbitration::compute_backoff_ms(new_count);

                Err(ErgataiError::LockConflictWithRetry {
                    file_path: file_path.to_string(),
                    message: format!("File {} lock conflict rejected by arbitration", file_path),
                    retry_after_ms,
                    retry_count: new_count,
                    max_retries: crate::file_access::conflict_arbitration::MAX_RETRIES,
                    priority_boosted: false,
                })
            }
        }
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

    /// Record a file access violation in the audit log.
    ///
    /// Called by `FileSystemWatcher` when a file modification is detected
    /// without a corresponding active lock. The agent/session are recorded
    /// as "unknown" since the watcher cannot identify the modifier.
    pub fn record_violation(
        &self,
        file_path: &str,
        action: &str,
    ) -> Result<(), ErgataiError> {
        let normalized_path = self
            .validate_and_normalize_path(file_path)
            .unwrap_or_else(|_| file_path.to_string());

        // Delegate to the shared audit logging method
        self.log_audit(
            "unknown",
            "unknown",
            action,
            Some(&normalized_path),
            None,
            Some(action),
        )
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
    /// - Canonicalizes the path (resolves symlinks, .., etc.)
    /// - Ensures it's within project root
    /// - Returns relative path from project root
    /// - M2 fix: Uses cached project_root_canonical to avoid repeated I/O
    ///
    /// Use this for WRITE operations where symlink safety is critical.
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
        // L10 fix: use fold to avoid intermediate Vec allocation
        let path_str = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .fold(String::new(), |mut acc, part| {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(&part);
                acc
            });

        Ok(path_str)
    }

    /// Validate that a scope pattern does not match more than `max_scope_size` files.
    ///
    /// Walks the project root and counts files matching the glob pattern. If the
    /// count exceeds the configured limit (default 1000), returns an error.
    /// This prevents overly broad scopes (e.g., `**`) from granting implicit
    /// access to the entire project.
    ///
    /// Optimization: for scopes that are a specific file path (no glob characters),
    /// the count is trivially 1, so we skip the filesystem walk.
    fn validate_scope_size(&self, scope: &str) -> Result<(), ErgataiError> {
        // Fast path: if the scope has no glob characters, it's a single file
        if !scope.contains('*') && !scope.contains('?') && !scope.contains('[') {
            return Ok(());
        }

        // Default limit (if no config manager is available)
        let max_files: u64 = 1000;

        // Validate pattern syntax first (glob::glob below re-parses, but we want
        // to catch invalid patterns here with a clear error message)
        let _validated_pattern = glob::Pattern::new(scope).map_err(|e| {
            ErgataiError::InvalidArgument(format!("Invalid scope glob pattern '{}': {}", scope, e))
        })?;

        let mut count: u64 = 0;
        let walker = glob::glob(self.project_root.join(scope).to_string_lossy().as_ref())
            .map_err(|e| ErgataiError::internal(format!("Failed to read glob pattern: {}", e)))?;

        for entry in walker {
            if entry.is_ok() {
                count += 1;
                if count > max_files {
                    return Err(ErgataiError::PermissionDenied(format!(
                        "Scope '{}' matches more than {} files (limit exceeded). Use a narrower scope.",
                        scope, max_files
                    )));
                }
            }
        }

        debug!(scope = scope, file_count = count, "Scope size validated");
        Ok(())
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
            .query_map(params![token_id], |row| {
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
            })
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

    /// Get an AuditManager sharing this manager's database connection.
    ///
    /// Useful for querying audit log entries after lock operations without
    /// requiring a separate connection.
    pub fn audit_manager(&self) -> AuditManager {
        AuditManager::new(Arc::clone(&self.conn))
    }

    /// Test helper: Set heartbeat to a past time for testing timeout scenarios
    #[cfg(test)]
    pub fn set_heartbeat_past(&self, token_id: &str, seconds_ago: i64) -> Result<(), ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        // Use strftime to format as RFC3339 (ISO 8601 with timezone)
        conn.execute(
            "UPDATE system_tokens SET heartbeat_at = strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now', ?1) WHERE id = ?2",
            rusqlite::params![format!("-{} seconds", seconds_ago), token_id],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to update heartbeat: {}", e)))?;

        Ok(())
    }

    /// Get all file tokens for a given system token ID
    pub fn get_file_tokens_by_system_token(&self, system_token_id: &str) -> Result<Vec<FileToken>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, session_id, system_token_id, scope, mode, reason,
                        approved_by, issued_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_tokens
                 WHERE system_token_id = ?1 AND status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let tokens = stmt
            .query_map(params![system_token_id], |row| {
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
                    heartbeat_interval_secs: row.get::<_, i64>(10)? as u64,
                    heartbeat_at: parse_datetime(&row.get::<_, String>(11)?),
                    status: parse_token_status(&row.get::<_, String>(12)?),
                    priority: None,
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query file tokens: {}", e)))?;

        let mut result = Vec::new();
        for token in tokens {
            result.push(token.map_err(|e| ErgataiError::internal(format!("Failed to parse file token: {}", e)))?);
        }

        Ok(result)
    }

    /// Test helper: Get all active file locks
    #[cfg(test)]
    pub fn get_all_active_locks(&self) -> Result<Vec<FileLock>, ErgataiError> {
        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, agent_id, session_id, mode, scope, token_id,
                        reason, approved_by, created_at, expires_at, heartbeat_interval_secs,
                        heartbeat_at, status
                 FROM file_locks
                 WHERE status = 'ACTIVE'",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let locks = stmt
            .query_map(params![], parse_file_lock_row)
            .map_err(|e| ErgataiError::internal(format!("Failed to query locks: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect locks: {}", e)))?;

        Ok(locks)
    }

    /// Acquire a lock with automatic waiting using NATS queue
    ///
    /// This method attempts to acquire a lock immediately. If the lock is already held,
    /// it publishes a request to the NATS LOCK_WAITERS queue and waits for a notification.
    ///
    /// # Arguments
    /// * `token` - The file token requesting the lock
    /// * `file_path` - The file path to lock
    /// * `nats_connection` - NATS connection for publishing wait requests
    /// * `timeout` - Maximum time to wait for the lock
    ///
    /// # Returns
    /// * `Ok(())` if the lock was acquired (immediately or after waiting)
    /// * `Err(ErgataiError::Timeout)` if the timeout was reached
    /// * `Err(ErgataiError)` for other errors
    ///
    /// # Example
    /// ```rust,ignore
    /// let nats = nats_connection.clone();
    /// let timeout = Duration::from_secs(30);
    /// lock_manager.acquire_lock_with_wait(&token, "main.rs", nats, timeout).await?;
    /// // Lock is now held, proceed with file operations
    /// ```
    pub async fn acquire_lock_with_wait(
        &self,
        token: &FileToken,
        file_path: &str,
        nats_connection: Arc<crate::nats::connection::NatsConnection>,
        timeout: std::time::Duration,
    ) -> Result<(), ErgataiError> {
        use crate::file_access::lock_waiter::{LockWaitRequest, LockGrantedNotification};
        use futures_util::StreamExt;

        // Try to acquire immediately
        match self.acquire_lock(token, file_path).await {
            Ok(()) => {
                tracing::debug!(
                    file_path = file_path,
                    agent_id = %token.agent_id,
                    "Lock acquired immediately"
                );
                return Ok(());
            }
            Err(ErgataiError::LockConflict(_)) => {
                // Lock is held, need to wait
                tracing::info!(
                    file_path = file_path,
                    agent_id = %token.agent_id,
                    "Lock conflict, joining wait queue"
                );
            }
            Err(e) => return Err(e),
        }

        // Create wait request
        let wait_request = LockWaitRequest::new(
            token.id.as_str().to_string(),
            token.agent_id.clone(),
            token.session_id.clone(),
            file_path.to_string(),
            token.mode.clone(),
            None, // No priority for now
        );

        let request_id = wait_request.request_id.clone();
        let reply_subject = wait_request.reply_subject.clone();

        // Publish to NATS
        let subject = wait_request.subject();
        let payload = serde_json::to_vec(&wait_request)
            .map_err(|e| ErgataiError::internal(format!("Failed to serialize wait request: {}", e)))?;

        nats_connection.publish(&subject, payload).await
            .map_err(|e| ErgataiError::internal(format!("Failed to publish wait request: {}", e)))?;

        tracing::debug!(
            request_id = %request_id,
            file_path = file_path,
            "Published lock wait request to NATS"
        );

        // Subscribe to reply subject
        let mut subscriber = nats_connection.client().subscribe(reply_subject.clone()).await
            .map_err(|e| ErgataiError::internal(format!("Failed to subscribe to reply subject: {}", e)))?;

        // Wait for notification with timeout
        let notification = tokio::time::timeout(timeout, async {
            while let Some(msg) = subscriber.next().await {
                if let Ok(grant) = serde_json::from_slice::<LockGrantedNotification>(&msg.payload) {
                    if grant.request_id == request_id {
                        return Some(grant);
                    }
                }
            }
            None
        })
        .await;

        match notification {
            Ok(Some(_grant)) => {
                tracing::info!(
                    request_id = %request_id,
                    file_path = file_path,
                    "Received lock grant notification"
                );

                // Try to acquire the lock now
                match self.acquire_lock(token, file_path).await {
                    Ok(()) => {
                        tracing::info!(
                            file_path = file_path,
                            agent_id = %token.agent_id,
                            "Lock acquired after waiting"
                        );
                        Ok(())
                    }
                    Err(e) => {
                        tracing::error!(
                            file_path = file_path,
                            agent_id = %token.agent_id,
                            error = %e,
                            "Failed to acquire lock after grant notification"
                        );
                        Err(e)
                    }
                }
            }
            Ok(None) => {
                tracing::warn!(
                    request_id = %request_id,
                    file_path = file_path,
                    "Subscriber closed before receiving grant"
                );
                Err(ErgataiError::internal("Lock wait channel closed unexpectedly"))
            }
            Err(_) => {
                tracing::warn!(
                    request_id = %request_id,
                    file_path = file_path,
                    timeout_secs = timeout.as_secs(),
                    "Lock acquisition timed out"
                );
                Err(ErgataiError::internal(format!(
                    "Lock acquisition timed out after {} seconds",
                    timeout.as_secs()
                )))
            }
        }
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
                        heartbeat_at, status, priority
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
                    priority: row.get::<_, Option<i64>>(13)?.map(|p| p as u8),
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
        // Validate scope size (M9 fix): count matching files and reject if over limit
        self.validate_scope_size(&token.scope)?;

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute(
            "INSERT INTO file_tokens (
                id, agent_id, session_id, system_token_id, scope, mode, reason,
                approved_by, issued_at, expires_at, heartbeat_interval_secs,
                heartbeat_at, status, priority
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                token.priority.map(|p| p as i64),
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
                        heartbeat_at, status, priority
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
                    priority: row.get::<_, Option<i64>>(13)?.map(|p| p as u8),
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
    ///
    /// Read the latest content of a file, waiting for any pending WRITE to complete.
    ///
    /// Waits for any pending WRITE to complete or fail before reading.
    /// Returns the file content as bytes.
    ///
    /// # Safety Note
    /// This method does NOT hold the waiters lock while checking the lock state,
    /// because `is_file_locked_for_write` acquires `conn` lock. Holding `waiters`
    /// while acquiring `conn` would create a lock ordering inversion with
    /// `release_lock` (which does `conn` → `waiters`), causing deadlock.
    ///
    /// There is a minor TOCTOU race: if WRITE completes between the check and
    /// waiter registration, the waiter blocks up to 30s timeout. This is safe
    /// (not a deadlock) and rare in practice.
    pub async fn read_latest(&self, file_path: &str) -> Result<Vec<u8>, ErgataiError> {
        // Check if file is locked for WRITE (acquires/releases conn lock)
        if self.is_file_locked_for_write(file_path)? {
            // Register waiter (acquires waiters lock — no conn lock held, avoids deadlock)
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

    // ─── Single-agent mode detection ─────────────────────────────────────

    /// Register a new active ACP session.
    ///
    /// Called when an ACP session is created (system token issued). Updates the
    /// active session count and resets the single-agent stabilization timer.
    /// If the session was recently disconnected (within SESSION_STICKINESS_SECS),
    /// the disconnect is treated as transient and cleared.
    pub fn register_session(&self) {
        let prev = self.active_session_count.fetch_add(1, Ordering::Relaxed);
        // Reset hysteresis timer — count changed
        if let Ok(mut guard) = self.single_agent_since.lock() {
            *guard = None;
        }
        // Note: we don't know the session_id here, so we clear all expired
        // disconnected sessions. The reconnect logic in is_single_agent_mode
        // handles the actual session-aware stickiness.
        self.cleanup_disconnected_sessions();
        info!(
            prev_count = prev,
            new_count = prev + 1,
            "ACP session registered"
        );
    }

    /// Register a specific session by ID, clearing its disconnected status.
    ///
    /// Call this when you know the session_id that is reconnecting. If the session
    /// was marked as temporarily_disconnected, the mark is cleared.
    pub fn register_session_with_id(&self, session_id: &str) {
        // Clear disconnected mark for this specific session
        if let Ok(mut guard) = self.disconnected_sessions.lock() {
            let was_disconnected = guard.remove(session_id).is_some();
            if was_disconnected {
                info!(session_id = session_id, "Session reconnected within stickiness window");
            }
        }
        self.register_session();
    }

    /// Unregister an active ACP session.
    ///
    /// Called when an ACP session ends (system token revoked or expired).
    /// Updates the active session count and resets the single-agent stabilization timer.
    /// The session is marked as temporarily_disconnected for SESSION_STICKINESS_SECS.
    pub fn unregister_session(&self) {
        // Use fetch_update for atomic saturating subtract (prevents lost-update race with fetch_add)
        let prev = self.active_session_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |val| Some(val.saturating_sub(1)))
            .unwrap_or(0);
        let new = prev.saturating_sub(1);
        // Reset hysteresis timer — count changed
        if let Ok(mut guard) = self.single_agent_since.lock() {
            *guard = None;
        }
        info!(
            prev_count = prev,
            new_count = new,
            "ACP session unregistered"
        );
    }

    /// Unregister a specific session by ID, marking it as temporarily disconnected.
    ///
    /// The session will be considered "temporarily disconnected" for
    /// `SESSION_STICKINESS_SECS` seconds. If it reconnects within that window,
    /// single-agent mode detection treats the disconnect as transient.
    pub fn unregister_session_with_id(&self, session_id: &str) {
        // Mark as temporarily disconnected
        if let Ok(mut guard) = self.disconnected_sessions.lock() {
            guard.insert(session_id.to_string(), Instant::now());
        }
        self.unregister_session();
    }

    /// Clean up expired disconnected sessions (older than SESSION_STICKINESS_SECS).
    fn cleanup_disconnected_sessions(&self) {
        if let Ok(mut guard) = self.disconnected_sessions.lock() {
            guard.retain(|_, instant| instant.elapsed().as_secs() < SESSION_STICKINESS_SECS);
        }
    }

    /// Count how many sessions are currently in the temporarily_disconnected state.
    fn recently_disconnected_count(&self) -> usize {
        self.cleanup_disconnected_sessions();
        self.disconnected_sessions
            .lock()
            .map(|g| g.len())
            .unwrap_or(0)
    }

    /// Check whether the system is in single-agent mode.
    ///
    /// Returns `true` only when:
    /// 1. Exactly one ACP session is currently active, AND
    /// 2. No sessions are in the "temporarily disconnected" state (session stickiness), AND
    /// 3. The count has remained at 1 for at least `SINGLE_AGENT_STABILIZE_SECS`
    ///    consecutive seconds (hysteresis).
    ///
    /// This prevents rapid toggling when agents connect/disconnect in quick
    /// succession. The approval flow is bypassed in single-agent mode because
    /// there is no contention risk.
    ///
    /// **Session stickiness**: If a session disconnected within the last
    /// `SESSION_STICKINESS_SECS` seconds, the system does NOT enter single-agent
    /// mode, because the disconnected session may reconnect. This prevents the
    /// main agent's temporary disconnect from triggering approval bypass.
    pub fn is_single_agent_mode(&self) -> bool {
        let count = self.active_session_count.load(Ordering::Relaxed);

        // If there are recently disconnected sessions, we are NOT in single-agent
        // mode — the disconnected session(s) may reconnect at any time.
        let disconnected = self.recently_disconnected_count();
        let effective_count = count + disconnected;

        if effective_count != 1 {
            // Not single-agent — ensure timer is reset
            if let Ok(mut guard) = self.single_agent_since.lock() {
                if guard.is_some() {
                    *guard = None;
                }
            }
            return false;
        }

        // Count is 1 (and no disconnected sessions) — check or start the stabilization timer
        let mut guard = match self.single_agent_since.lock() {
            Ok(g) => g,
            Err(_) => return false, // Poisoned lock — fail safe (don't bypass)
        };

        match *guard {
            Some(since) => {
                let elapsed = since.elapsed().as_secs();
                if elapsed >= SINGLE_AGENT_STABILIZE_SECS {
                    debug!(
                        elapsed_secs = elapsed,
                        "Single-agent mode active (approval bypassed)"
                    );
                    true
                } else {
                    debug!(
                        elapsed_secs = elapsed,
                        stabilize_secs = SINGLE_AGENT_STABILIZE_SECS,
                        "Single-agent detected, stabilizing..."
                    );
                    false
                }
            }
            None => {
                // First observation of count==1 — start timer
                *guard = Some(Instant::now());
                debug!("Single-agent first detected, starting stabilization timer");
                false
            }
        }
    }

    /// Get the current active session count (for diagnostics / NAPI exposure).
    pub fn active_session_count(&self) -> usize {
        self.active_session_count.load(Ordering::Relaxed)
    }

    /// Upgrade an existing READ lock to WRITE on the same file.
    ///
    /// This implements the deadlock-safe upgrade pattern from the architecture plan:
    /// 1. Verify current READ lock exists for this token + file
    /// 2. Release the READ lock (so other agents can proceed)
    /// 3. Attempt to acquire WRITE via the normal `acquire_lock` path (respects
    ///    single-agent mode, arbitration, retry tracker)
    /// 4. If WRITE acquisition fails → re-acquire READ to restore prior state
    ///
    /// This avoids the in-place UPDATE deadlock that occurs when multiple agents
    /// try to upgrade READ→WRITE simultaneously on the same file.
    pub async fn upgrade_to_write(&self, token: &FileToken, file_path: &str) -> Result<(), ErgataiError> {
        info!(
            token_id = %token.id,
            file_path = file_path,
            agent_id = %token.agent_id,
            "Upgrading lock from READ to WRITE (deadlock-safe)"
        );

        // Step 1: Verify current READ lock exists
        {
            let conn = self.conn.lock().map_err(|e| {
                ErgataiError::internal(format!("Failed to acquire lock: {}", e))
            })?;
            let mode: Option<String> = match conn.query_row(
                "SELECT mode FROM file_locks
                 WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
                 LIMIT 1",
                params![token.id.as_str(), file_path],
                |row| row.get(0),
            ) {
                Ok(m) => Some(m),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    return Err(ErgataiError::internal(format!(
                        "Failed to query lock mode for upgrade: {}", e
                    )));
                }
            };

            match mode.as_deref() {
                Some("READ") => { /* good, proceed */ }
                Some("WRITE") => {
                    return Ok(()); // Already WRITE, no-op
                }
                Some(other) => {
                    return Err(ErgataiError::InvalidArgument(format!(
                        "Cannot upgrade lock in {} mode (expected READ)", other
                    )));
                }
                None => {
                    return Err(ErgataiError::NotFound(format!(
                        "No active READ lock found for token {} on file {}",
                        token.id, file_path
                    )));
                }
            }
        }

        // Step 2: Release the READ lock
        self.release_lock(token.id.as_str(), file_path).await?;

        // Step 3: Attempt WRITE acquisition via a temporary WRITE-mode token
        //
        // We create a synthetic FileToken with WRITE mode so acquire_lock's
        // conflict check triggers. All arbitration / single-agent bypass /
        // retry tracking flows through acquire_lock normally.
        let write_token = FileToken::new(
            token.agent_id.clone(),
            token.session_id.clone(),
            token.system_token_id.clone(),
            token.scope.clone(),
            FileMode::Write,
            token.reason.clone(),
            token.approved_by.clone(),
            // Compute remaining TTL
            {
                let now = chrono::Utc::now();
                let remaining = token.expires_at.signed_duration_since(now);
                remaining.num_seconds().max(60) as u64
            },
            token.heartbeat_interval_secs,
        );

        match self.acquire_lock(&write_token, file_path).await {
            Ok(()) => {
                info!(
                    token_id = %token.id,
                    file_path = file_path,
                    "Lock upgraded from READ to WRITE successfully"
                );
                Ok(())
            }
            Err(e) => {
                // Step 4: Failed to get WRITE — restore READ lock
                warn!(
                    token_id = %token.id,
                    file_path = file_path,
                    error = %e,
                    "WRITE acquisition failed during upgrade, restoring READ lock"
                );

                let read_token = FileToken::new(
                    token.agent_id.clone(),
                    token.session_id.clone(),
                    token.system_token_id.clone(),
                    token.scope.clone(),
                    FileMode::Read,
                    token.reason.clone(),
                    token.approved_by.clone(),
                    {
                        let now = chrono::Utc::now();
                        let remaining = token.expires_at.signed_duration_since(now);
                        remaining.num_seconds().max(60) as u64
                    },
                    token.heartbeat_interval_secs,
                );

                if let Err(restore_err) = self.acquire_lock(&read_token, file_path).await {
                    tracing::error!(
                        token_id = %token.id,
                        file_path = file_path,
                        error = %restore_err,
                        "CRITICAL: Failed to restore READ lock after upgrade failure"
                    );
                }

                Err(e)
            }
        }
    }

    /// Downgrade an existing WRITE lock to READ on the same file.
    ///
    /// Safer than upgrade (no contention risk) — just updates the lock mode.
    pub fn downgrade_to_read(&self, token: &FileToken, file_path: &str) -> Result<(), ErgataiError> {
        info!(
            token_id = %token.id,
            file_path = file_path,
            agent_id = %token.agent_id,
            "Downgrading lock from WRITE to READ"
        );

        let conn = self.conn.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to acquire lock: {}", e))
        })?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| ErgataiError::internal(format!("Failed to begin transaction: {}", e)))?;

        // Verify current WRITE lock exists
        let mode: Option<String> = match conn.query_row(
            "SELECT mode FROM file_locks
             WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'
             LIMIT 1",
            params![token.id.as_str(), file_path],
            |row| row.get(0),
        ) {
            Ok(m) => Some(m),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                conn.execute_batch("ROLLBACK").ok();
                return Err(ErgataiError::internal(format!(
                    "Failed to query lock mode for downgrade: {}", e
                )));
            }
        };

        match mode.as_deref() {
            Some("WRITE") | Some("ADMIN") => {
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "UPDATE file_locks SET mode = 'READ', updated_at = ?1
                     WHERE token_id = ?2 AND file_path = ?3 AND status = 'ACTIVE'",
                    params![now, token.id.as_str(), file_path],
                )
                .map_err(|e| {
                    conn.execute_batch("ROLLBACK").ok();
                    ErgataiError::internal(format!("Failed to downgrade lock: {}", e))
                })?;

                conn.execute_batch("COMMIT")
                    .map_err(|e| ErgataiError::internal(format!("Failed to commit downgrade: {}", e)))?;

                info!(token_id = %token.id, file_path = file_path, "Lock downgraded to READ");
                Ok(())
            }
            Some("READ") => {
                conn.execute_batch("ROLLBACK").ok();
                Ok(()) // Already READ, no-op
            }
            Some(other) => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::InvalidArgument(format!(
                    "Cannot downgrade lock in {} mode", other
                )))
            }
            None => {
                conn.execute_batch("ROLLBACK").ok();
                Err(ErgataiError::NotFound(format!(
                    "No active lock found for token {} on file {}",
                    token.id, file_path
                )))
            }
        }
    }

    // ===== NATS Approval Integration =====

    /// Subscribe to NATS subjects for approval responses.
    ///
    /// Starts a background task that listens for approval/reject messages from the main agent.
    /// This method should be called after initialization if NATS client is available.
    pub async fn subscribe_to_nats(&self) -> Result<(), ErgataiError> {
        let client = match &self.nats_client {
            Some(c) => c.clone(),
            None => {
                debug!("NATS client not available, skipping subscription (degraded mode)");
                return Ok(());
            }
        };

        info!("Subscribing to NATS approval responses");

        // Subscribe to approval and reject subjects
        let mut approve_subscriber = client
            .subscribe("ergatai.file.access.approve")
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to subscribe to approve subject: {}", e)))?;

        let mut reject_subscriber = client
            .subscribe("ergatai.file.access.reject")
            .await
            .map_err(|e| ErgataiError::internal(format!("Failed to subscribe to reject subject: {}", e)))?;

        let pending = self.pending_approvals.clone();

        // Spawn background task to process approval responses
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Handle approval messages
                    Some(msg) = approve_subscriber.next() => {
                        Self::handle_approval_response(msg, pending.clone(), true).await;
                    }
                    // Handle rejection messages
                    Some(msg) = reject_subscriber.next() => {
                        Self::handle_approval_response(msg, pending.clone(), false).await;
                    }
                    // Exit if both subscribers close
                    else => break,
                }
            }
            info!("NATS approval subscription task ended");
        });

        // Store the task handle for cleanup
        let mut task_guard = self.subscription_task.lock().map_err(|e| {
            ErgataiError::internal(format!("Failed to lock subscription task: {}", e))
        })?;
        *task_guard = Some(task);

        info!("Successfully subscribed to NATS approval subjects");
        Ok(())
    }

    /// Handle an approval or rejection response from the main agent.
    async fn handle_approval_response(
        msg: async_nats::Message,
        pending: Arc<Mutex<HashMap<String, oneshot::Sender<WriteConflictApproval>>>>,
        is_approval: bool,
    ) {
        debug!("Received NATS message on subject: {}", msg.subject.as_str());

        // Parse the payload and extract both response and request_id in one pass
        let (response, request_id) = if is_approval {
            let Ok(payload) = serde_json::from_slice::<FileAccessApprovePayload>(&msg.payload) else {
                error!("Failed to parse FileAccessApprovePayload");
                return;
            };
            let response = WriteConflictApproval {
                approved: true,
                approved_by: payload.approver_id,
                reason: payload.custom_scope.map(|s| format!("Custom scope: {}", s)),
            };
            (response, payload.request_id)
        } else {
            let Ok(payload) = serde_json::from_slice::<FileAccessRejectPayload>(&msg.payload) else {
                error!("Failed to parse FileAccessRejectPayload");
                return;
            };
            let response = WriteConflictApproval {
                approved: false,
                approved_by: payload.rejecter_id,
                reason: Some(payload.reason),
            };
            (response, payload.request_id)
        };

        // Minimize Mutex critical section: extract sender first, then release lock before logging/sending
        let sender = {
            // Poison-safe Mutex handling: recover data even if a previous holder panicked
            let mut waiters = pending.lock().unwrap_or_else(|e| {
                error!("Mutex poisoned, recovering: {}", e);
                e.into_inner()
            });
            waiters.remove(&request_id)
        }; // lock released here

        if let Some(tx) = sender {
            info!(request_id = %request_id, approved = %response.approved, "Waking up approval waiter");
            let _ = tx.send(response);
        } else {
            warn!(request_id = %request_id, "No waiter found for request_id (may have timed out)");
        }
    }

    /// Check if escalation to main agent is needed.
    ///
    /// Returns true if:
    /// - NOT in single-agent mode (multiple agents active)
    /// - NATS client is available
    pub fn should_escalate_to_main_agent(&self) -> bool {
        !self.is_single_agent_mode() && self.nats_client.is_some()
    }

    /// Request approval from main agent via NATS.
    ///
    /// Sends an escalation request and returns a request_id for tracking.
    /// **Idempotent**: if the same (agent_id, file_path, mode) request was already
    /// sent within the last 30 seconds and is still pending, the existing request_id
    /// is returned without sending a duplicate NATS message.
    pub async fn request_approval_from_main_agent(
        &self,
        token: &FileToken,
        file_path: &str,
        conflict_with: Option<&str>,
        reason: &str,
    ) -> Result<String, ErgataiError> {
        // Idempotency check: reuse existing pending request if available
        let idempotency_key = format!("{}:{}:{}", token.agent_id, file_path, token.mode);

        // Clean up stale entries and check for existing request
        {
            let mut guard = self.pending_request_keys.lock().map_err(|e| {
                ErgataiError::internal(format!("Failed to lock pending_request_keys: {}", e))
            })?;
            // Remove entries older than 60s
            guard.retain(|_, (_, instant)| instant.elapsed().as_secs() < 60);

            if let Some((existing_id, instant)) = guard.get(&idempotency_key) {
                if instant.elapsed().as_secs() < 30 {
                    debug!(
                        idempotency_key = %idempotency_key,
                        existing_request_id = %existing_id,
                        "Reusing existing pending approval request (idempotent)"
                    );
                    return Ok(existing_id.clone());
                }
            }
        }

        let request_id = format!("approval-{}", uuid::Uuid::new_v4());

        let payload = FileAccessEscalatePayload {
            request_id: request_id.clone(),
            agent_id: token.agent_id.clone(),
            file_path: file_path.to_string(),
            mode: token.mode.to_string(),
            reason: Some(reason.to_string()),
            conflict_with: conflict_with.map(|s| s.to_string()),
            timeout_secs: 30,
            timestamp: Utc::now().timestamp() as u64,
        };

        if let Some(client) = &self.nats_client {
            let subject = "ergatai.file.access.escalate.main";
            let payload_bytes = serde_json::to_vec(&payload)
                .map_err(|e| ErgataiError::internal(format!("Failed to serialize payload: {}", e)))?;

            client
                .publish(subject, payload_bytes.into())
                .await
                .map_err(|e| ErgataiError::internal(format!("Failed to publish escalation request: {}", e)))?;

            // Record the request for idempotency
            if let Ok(mut guard) = self.pending_request_keys.lock() {
                guard.insert(idempotency_key, (request_id.clone(), Instant::now()));
            }

            info!(
                request_id = %request_id,
                agent_id = %token.agent_id,
                file_path = file_path,
                "Sent approval request to main agent"
            );
        } else {
            return Err(ErgataiError::internal("NATS client not available"));
        }

        Ok(request_id)
    }

    /// Wait for approval response from main agent with timeout.
    ///
    /// Registers a oneshot channel and waits for the response.
    /// Returns default deny on timeout.
    pub async fn wait_for_approval(
        &self,
        request_id: &str,
        timeout_duration: Duration,
    ) -> Result<WriteConflictApproval, ErgataiError> {
        let (tx, rx) = oneshot::channel();

        // Register the waiter
        {
            let mut waiters = self.pending_approvals.lock().map_err(|e| {
                ErgataiError::internal(format!("Failed to lock pending approvals: {}", e))
            })?;
            waiters.insert(request_id.to_string(), tx);
        }

        debug!(request_id = %request_id, "Waiting for approval response");

        // Wait for response with timeout
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(response)) => {
                info!(request_id = %request_id, approved = %response.approved, "Received approval response");
                Ok(response)
            }
            Ok(Err(_)) => {
                // Channel dropped without response
                warn!(request_id = %request_id, "Approval channel dropped");
                Err(ErgataiError::internal("Approval channel closed"))
            }
            Err(_) => {
                // Timeout - remove waiter and return default deny
                let mut waiters = self.pending_approvals.lock().map_err(|e| {
                    ErgataiError::internal(format!("Failed to lock pending approvals: {}", e))
                })?;
                waiters.remove(request_id);

                warn!(request_id = %request_id, "Approval request timed out");
                Err(ErgataiError::AgentTimeout {
                    message: format!("Approval request timed out after {} seconds", timeout_duration.as_secs()),
                    source: None,
                })
            }
        }
    }

    /// Shutdown NATS subscription task.
    ///
    /// Called during cleanup to gracefully stop the background subscription.
    pub fn shutdown_nats_subscription(&self) {
        if let Ok(mut task_guard) = self.subscription_task.lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
                info!("NATS approval subscription task aborted");
            }
        }
    }
}

// Helper functions for parsing database values

fn parse_file_mode(s: &str) -> FileMode {
    // L9 fix: use eq_ignore_ascii_case to avoid allocation from to_uppercase()
    if s.eq_ignore_ascii_case("READ") {
        FileMode::Read
    } else if s.eq_ignore_ascii_case("WRITE") {
        FileMode::Write
    } else if s.eq_ignore_ascii_case("ADMIN") {
        FileMode::Admin
    } else {
        tracing::warn!(mode = s, "Unknown file mode in DB, defaulting to Read (least privilege)");
        FileMode::Read
    }
}

fn parse_token_status(s: &str) -> TokenStatus {
    if s.eq_ignore_ascii_case("ACTIVE") {
        TokenStatus::Active
    } else if s.eq_ignore_ascii_case("UPGRADING") {
        TokenStatus::Upgrading
    } else if s.eq_ignore_ascii_case("EXPIRED") {
        TokenStatus::Expired
    } else if s.eq_ignore_ascii_case("REVOKED") {
        TokenStatus::Revoked
    } else {
        tracing::warn!(status = s, "Unknown token status in DB, defaulting to Expired (fail-safe)");
        TokenStatus::Expired
    }
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    match DateTime::parse_from_rfc3339(s) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(e) => {
            tracing::error!(raw = s, error = %e, "Invalid datetime in DB, using UNIX_EPOCH (fail-safe: expired)");
            DateTime::UNIX_EPOCH
        }
    }
}

/// Parse a database row into a FileLock struct.
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

        let manager = FileLockManager::new(&db_path, project_root, None).unwrap();
        (manager, temp_dir)
    }

    #[tokio::test]
    async fn test_wal_mode_enabled() {
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
        manager.acquire_lock(&file_token, "test.txt").await.unwrap();

        // Check lock exists
        assert!(manager.is_file_locked("test.txt").unwrap());

        // Release lock
        manager.release_lock(file_token.id.as_str(), "test.txt").unwrap();

        // Check lock released
        assert!(!manager.is_file_locked("test.txt").unwrap());
    }

    #[tokio::test]
    async fn test_write_conflict_detection() {
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
        manager.acquire_lock(&token1, "test.txt").await.unwrap();

        // Second agent should fail (conflict) — now returns LockConflictWithRetry
        let result = manager.acquire_lock(&token2, "test.txt");
        assert!(matches!(result.await, Err(ErgataiError::LockConflict(_)) | Err(ErgataiError::LockConflictWithRetry { .. })));
    }

    #[tokio::test]
    async fn test_path_traversal_rejected() {
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
        assert!(matches!(result.await, Err(ErgataiError::InvalidPath(_))));
    }

    #[tokio::test]
    async fn test_single_agent_mode_stabilization() {
        use std::thread;
        use std::time::Duration;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();

        // Initially: no sessions → not single-agent
        assert!(!manager.is_single_agent_mode());
        assert_eq!(manager.active_session_count(), 0);

        // Register one session → count=1, but not yet stabilized
        manager.register_session();
        assert_eq!(manager.active_session_count(), 1);
        assert!(!manager.is_single_agent_mode()); // First call starts timer

        // Still stabilizing (check immediately)
        assert!(!manager.is_single_agent_mode());

        // Register a second session → count=2, timer reset
        manager.register_session();
        assert_eq!(manager.active_session_count(), 2);
        assert!(!manager.is_single_agent_mode());

        // Back to 1 session → timer restarts
        manager.unregister_session();
        assert_eq!(manager.active_session_count(), 1);
        assert!(!manager.is_single_agent_mode()); // Timer just restarted

        // Wait for stabilization
        thread::sleep(Duration::from_secs(6));
        assert!(manager.is_single_agent_mode()); // Now stabilized

        // Unregister all → not single-agent
        manager.unregister_session();
        assert_eq!(manager.active_session_count(), 0);
        assert!(!manager.is_single_agent_mode());
    }

    #[tokio::test]
    async fn test_unregister_saturating_sub() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();

        // Unregister without register should not underflow
        manager.unregister_session();
        assert_eq!(manager.active_session_count(), 0);
    }

    #[tokio::test]
    async fn test_upgrade_read_to_write_basic() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();
        // Create the file so acquire_lock can canonicalize it
        std::fs::write(temp_dir.path().join("test.rs"), "content").unwrap();

        // Set up: register system token, create READ token, acquire READ lock
        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );
        manager.register_system_token(&system_token).unwrap();

        let read_token = FileToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        manager.register_file_token(&read_token).unwrap();
        manager.acquire_lock(&read_token, "test.rs").await.unwrap();

        // Verify READ lock exists (check DB)
        {
            let conn = manager.conn.lock().unwrap();
            let mode: String = conn.query_row(
                "SELECT mode FROM file_locks WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'",
                params![read_token.id.as_str(), "test.rs"],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(mode, "READ");
        }

        // Upgrade to WRITE
        manager.upgrade_to_write(&read_token, "test.rs").await.unwrap();

        // Verify a WRITE lock now exists on the file.
        // upgrade_to_write releases the READ lock (original token_id → RELEASED)
        // and creates a new synthetic WRITE token (new token_id → ACTIVE).
        // So we query for any ACTIVE lock on this file with agent_id = "test-agent".
        let conn = manager.conn.lock().unwrap();
        let active_write: Vec<(String, String)> = conn.prepare(
            "SELECT token_id, mode FROM file_locks
             WHERE file_path = ?1 AND agent_id = ?2 AND status = 'ACTIVE'"
        )
        .unwrap()
        .query_map(params!["test.rs", "test-agent"], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

        assert_eq!(active_write.len(), 1);
        assert_eq!(active_write[0].1, "WRITE");
        // Original token should no longer be ACTIVE (it was released during upgrade)
        let orig_status: String = conn.query_row(
            "SELECT status FROM file_locks WHERE token_id = ?1 AND file_path = ?2",
            params![read_token.id.as_str(), "test.rs"],
            |row| row.get(0),
        ).unwrap();
        assert_ne!(orig_status, "ACTIVE", "Original READ lock should not be ACTIVE after upgrade");
    }

    #[tokio::test]
    async fn test_upgrade_to_write_no_read_lock_fails() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();
        std::fs::write(temp_dir.path().join("test.rs"), "content").unwrap();

        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );
        manager.register_system_token(&system_token).unwrap();

        let read_token = FileToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            system_token.id.clone(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        manager.register_file_token(&read_token).unwrap();

        // Try to upgrade without holding a lock → should fail
        let result = manager.upgrade_to_write(&read_token, "test.rs");
        assert!(matches!(result.await, Err(ErgataiError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_upgrade_restores_read_on_conflict() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();
        std::fs::write(temp_dir.path().join("test.rs"), "content").unwrap();

        // Register two sessions so we're NOT in single-agent mode
        let sys1 = SystemToken::new(
            "agent-a".to_string(), "session-a".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600, 30,
        );
        let sys2 = SystemToken::new(
            "agent-b".to_string(), "session-b".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600, 30,
        );
        manager.register_system_token(&sys1).unwrap();
        manager.register_system_token(&sys2).unwrap();

        // Agent B holds WRITE lock on test.rs
        let write_token = FileToken::new(
            "agent-b".to_string(),
            "session-b".to_string(),
            sys2.id.clone(),
            "**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );
        manager.register_file_token(&write_token).unwrap();
        manager.acquire_lock(&write_token, "test.rs").await.unwrap();

        // Agent A holds READ lock on test.rs
        let read_token = FileToken::new(
            "agent-a".to_string(),
            "session-a".to_string(),
            sys1.id.clone(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        manager.register_file_token(&read_token).unwrap();
        manager.acquire_lock(&read_token, "test.rs").await.unwrap();

        // Agent A tries to upgrade to WRITE — should fail (B holds WRITE)
        let result = manager.upgrade_to_write(&read_token, "test.rs").await;
        assert!(result.is_err());

        // After failure, Agent A should still have a READ lock (restored).
        // The original READ lock was released, then re-acquired with a new token.
        // Check: at least one ACTIVE lock for agent-a on test.rs with mode READ.
        let conn = manager.conn.lock().unwrap();
        let restored: Vec<(String, String)> = conn.prepare(
            "SELECT token_id, mode FROM file_locks
             WHERE file_path = ?1 AND agent_id = ?2 AND status = 'ACTIVE'"
        )
        .unwrap()
        .query_map(params!["test.rs", "agent-a"], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

        // Should have exactly one restored READ lock
        assert!(restored.iter().any(|(_, m)| m == "READ"),
            "Expected a restored READ lock for agent-a on test.rs, got: {:?}", restored);
    }

    #[tokio::test]
    async fn test_downgrade_write_to_read() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let manager = FileLockManager::new(&db_path, temp_dir.path().to_path_buf(), None).unwrap();
        std::fs::write(temp_dir.path().join("test.rs"), "content").unwrap();

        let system_token = SystemToken::new(
            "test-agent".to_string(),
            "session-1".to_string(),
            manager.project_root.to_string_lossy().to_string(),
            3600,
            30,
        );
        manager.register_system_token(&system_token).unwrap();

        let write_token = FileToken::new(
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
        manager.register_file_token(&write_token).unwrap();
        manager.acquire_lock(&write_token, "test.rs").await.unwrap();

        // Downgrade to READ
        manager.downgrade_to_read(&write_token, "test.rs").unwrap();

        // Verify mode changed to READ
        let conn = manager.conn.lock().unwrap();
        let mode: String = conn.query_row(
            "SELECT mode FROM file_locks WHERE token_id = ?1 AND file_path = ?2 AND status = 'ACTIVE'",
            params![write_token.id.as_str(), "test.rs"],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(mode, "READ");
    }
}
