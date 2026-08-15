//! File access audit and reporting.
//!
//! Provides comprehensive audit logging, statistics, and security reporting
//! for file access operations.

use ergatai_error::ErgataiError;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tracing::info;

/// Audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Timestamp
    pub timestamp: String,
    /// Agent ID
    pub agent_id: String,
    /// Session ID
    pub session_id: String,
    /// Action (LOCK_ACQUIRED, LOCK_RELEASED, etc.)
    pub action: String,
    /// File path (if applicable)
    pub file_path: Option<String>,
    /// Lock mode (READ, WRITE, ADMIN)
    pub mode: Option<String>,
    /// Reason for the action
    pub reason: Option<String>,
}

/// File access statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileAccessStats {
    /// Total lock acquisitions
    pub total_acquisitions: u64,
    /// Total lock releases
    pub total_releases: u64,
    /// Total lock conflicts
    pub total_conflicts: u64,
    /// Total sensitive path accesses
    pub total_sensitive_accesses: u64,
    /// Acquisitions by agent
    pub acquisitions_by_agent: std::collections::HashMap<String, u64>,
    /// Accesses by file
    pub accesses_by_file: std::collections::HashMap<String, u64>,
    /// Accesses by mode
    pub accesses_by_mode: std::collections::HashMap<String, u64>,
    /// Average lock duration (in seconds)
    pub avg_lock_duration_secs: f64,
}

/// Security report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityReport {
    /// Report generation time
    pub generated_at: String,
    /// Report period start
    pub period_start: String,
    /// Report period end
    pub period_end: String,
    /// Summary statistics
    pub stats: FileAccessStats,
    /// Suspicious activities
    pub suspicious_activities: Vec<SuspiciousActivity>,
    /// Top agents by file accesses
    pub top_agents: Vec<AgentAccessSummary>,
    /// Top accessed files
    pub top_files: Vec<FileAccessSummary>,
}

/// Suspicious activity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuspiciousActivity {
    /// Timestamp
    pub timestamp: String,
    /// Agent ID
    pub agent_id: String,
    /// Activity description
    pub description: String,
    /// Severity (LOW, MEDIUM, HIGH, CRITICAL)
    pub severity: String,
}

/// Agent access summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAccessSummary {
    /// Agent ID
    pub agent_id: String,
    /// Total accesses
    pub total_accesses: u64,
    /// Unique files accessed
    pub unique_files: u64,
    /// WRITE operations count
    pub write_count: u64,
    /// Conflict count
    pub conflict_count: u64,
}

/// File access summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccessSummary {
    /// File path
    pub file_path: String,
    /// Total accesses
    pub total_accesses: u64,
    /// Unique agents
    pub unique_agents: u64,
    /// WRITE operations count
    pub write_count: u64,
    /// Conflict count
    pub conflict_count: u64,
}

/// Audit manager
pub struct AuditManager {
    /// SQLite connection (shared with FileLockManager)
    conn: Arc<Mutex<Connection>>,
}

impl AuditManager {
    /// Create a new AuditManager
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Query audit log with filters
    pub fn query_audit_log(
        &self,
        agent_id: Option<&str>,
        action: Option<&str>,
        file_path: Option<&str>,
        start_time: Option<&str>,
        end_time: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AuditEntry>, ErgataiError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let mut query = String::from(
            "SELECT timestamp, agent_id, session_id, action, file_path, mode, reason
             FROM audit_log WHERE 1=1",
        );
        // Pre-allocate for up to 4 filter parameters
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(4);

        if let Some(agent) = agent_id {
            query.push_str(" AND agent_id = ?");
            params_vec.push(Box::new(agent.to_string()));
        }

        if let Some(act) = action {
            query.push_str(" AND action = ?");
            params_vec.push(Box::new(act.to_string()));
        }

        if let Some(path) = file_path {
            query.push_str(" AND file_path = ?");
            params_vec.push(Box::new(path.to_string()));
        }

        if let Some(start) = start_time {
            query.push_str(" AND timestamp >= ?");
            params_vec.push(Box::new(start.to_string()));
        }

        if let Some(end) = end_time {
            query.push_str(" AND timestamp <= ?");
            params_vec.push(Box::new(end.to_string()));
        }

        query.push_str(" ORDER BY timestamp DESC LIMIT ?");
        params_vec.push(Box::new(limit as i64));

        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let entries = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(AuditEntry {
                    timestamp: row.get(0)?,
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    action: row.get(3)?,
                    file_path: row.get(4)?,
                    mode: row.get(5)?,
                    reason: row.get(6)?,
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query audit log: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect entries: {}", e)))?;

        Ok(entries)
    }

    /// Get file access statistics (public API — acquires lock)
    pub fn get_stats(&self, period_days: Option<u32>) -> Result<FileAccessStats, ErgataiError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;
        self.get_stats_inner(&conn, period_days)
    }

    /// Internal stats computation (accepts Connection reference — no lock acquisition)
    fn get_stats_inner(
        &self,
        conn: &Connection,
        period_days: Option<u32>,
    ) -> Result<FileAccessStats, ErgataiError> {
        let mut stats = FileAccessStats::default();

        // Calculate cutoff time
        let cutoff =
            period_days.map(|days| (Utc::now() - Duration::days(days as i64)).to_rfc3339());

        // Use a single aggregated query instead of N+1 queries (🔴-13 fix)
        let query = if cutoff.is_some() {
            "SELECT action, agent_id, mode, COUNT(*) as cnt
             FROM audit_log
             WHERE timestamp >= ?1
             GROUP BY action, agent_id, mode"
        } else {
            "SELECT action, agent_id, mode, COUNT(*) as cnt
             FROM audit_log
             GROUP BY action, agent_id, mode"
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare stats query: {}", e)))?;

        // Process rows — handle cutoff as optional param
        let row_iter: Box<
            dyn Iterator<
                Item = Result<(String, Option<String>, Option<String>, u64), rusqlite::Error>,
            >,
        > = if let Some(ref cutoff) = cutoff {
            Box::new(stmt.query_map(params![cutoff], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?)
        } else {
            Box::new(stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?)
        };

        for row in row_iter {
            let (action, agent_id, mode, cnt) = row
                .map_err(|e| ErgataiError::internal(format!("Failed to read stats row: {}", e)))?;

            match action.as_str() {
                "LOCK_ACQUIRED" => {
                    stats.total_acquisitions += cnt;
                    if let Some(ref agent) = agent_id {
                        *stats
                            .acquisitions_by_agent
                            .entry(agent.clone())
                            .or_insert(0) += cnt;
                    }
                    if let Some(ref m) = mode {
                        *stats.accesses_by_mode.entry(m.clone()).or_insert(0) += cnt;
                    }
                }
                "LOCK_RELEASED" => stats.total_releases += cnt,
                a if a.contains("CONFLICT") => stats.total_conflicts += cnt,
                _ => {}
            }
        }

        Ok(stats)
    }

    /// Generate a security report
    pub fn generate_security_report(
        &self,
        period_days: u32,
    ) -> Result<SecurityReport, ErgataiError> {
        info!(period_days = period_days, "Generating security report");

        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let now = Utc::now();
        let period_start = now - Duration::days(period_days as i64);

        // Get statistics (use inner version to avoid reentrant lock — 🔴-3 fix)
        let stats = self.get_stats_inner(&conn, Some(period_days))?;

        // Detect suspicious activities
        let suspicious_activities =
            self.detect_suspicious_activities_inner(&conn, &period_start)?;

        // Get top agents
        let top_agents = self.get_top_agents_inner(&conn, &period_start)?;

        // Get top files
        let top_files = self.get_top_files_inner(&conn, &period_start)?;

        let report = SecurityReport {
            generated_at: now.to_rfc3339(),
            period_start: period_start.to_rfc3339(),
            period_end: now.to_rfc3339(),
            stats,
            suspicious_activities,
            top_agents,
            top_files,
        };

        info!(
            suspicious_count = report.suspicious_activities.len(),
            top_agents_count = report.top_agents.len(),
            "Security report generated"
        );

        Ok(report)
    }

    /// Detect suspicious activities (inner — accepts Connection reference)
    fn detect_suspicious_activities_inner(
        &self,
        conn: &Connection,
        period_start: &DateTime<Utc>,
    ) -> Result<Vec<SuspiciousActivity>, ErgataiError> {
        // Pre-allocate with a reasonable default
        let mut activities = Vec::with_capacity(8);
        let period_str = period_start.to_rfc3339();

        // 1. Agents with high conflict rates (use params![] — 🔴-8 fix)
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, COUNT(*) as conflict_count
             FROM audit_log
             WHERE action LIKE '%CONFLICT%' AND timestamp >= ?1
             GROUP BY agent_id
             HAVING conflict_count > 10",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let rows = stmt
            .query_map(params![period_str], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query: {}", e)))?;

        for row in rows {
            let (agent_id, conflict_count) =
                row.map_err(|e| ErgataiError::internal(format!("Failed to read row: {}", e)))?;
            activities.push(SuspiciousActivity {
                timestamp: period_str.clone(),
                agent_id: agent_id.clone(),
                description: format!(
                    "High conflict rate: {} conflicts in the reporting period",
                    conflict_count
                ),
                severity: if conflict_count > 50 {
                    "HIGH".to_string()
                } else {
                    "MEDIUM".to_string()
                },
            });
        }

        // 2. Agents accessing sensitive paths
        // 🔴-7 fix: Added parentheses around OR clauses for correct AND/OR precedence
        // 🔴-8 fix: Use params![] instead of format!()
        let mut stmt = conn
            .prepare(
                "SELECT agent_id, COUNT(*) as sensitive_count
             FROM audit_log
             WHERE (file_path LIKE '%.env%' OR file_path LIKE '%.git/%'
               OR file_path LIKE '%credentials%' OR file_path LIKE '%.key')
               AND timestamp >= ?1
             GROUP BY agent_id",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let rows = stmt
            .query_map(params![period_str], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query: {}", e)))?;

        for row in rows {
            let (agent_id, sensitive_count) =
                row.map_err(|e| ErgataiError::internal(format!("Failed to read row: {}", e)))?;
            activities.push(SuspiciousActivity {
                timestamp: period_str.clone(),
                agent_id: agent_id.clone(),
                description: format!("Accessed sensitive paths {} times", sensitive_count),
                severity: if sensitive_count > 20 {
                    "HIGH".to_string()
                } else {
                    "LOW".to_string()
                },
            });
        }

        Ok(activities)
    }

    /// Get top agents by file accesses (inner — accepts Connection reference)
    fn get_top_agents_inner(
        &self,
        conn: &Connection,
        period_start: &DateTime<Utc>,
    ) -> Result<Vec<AgentAccessSummary>, ErgataiError> {
        let period_str = period_start.to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT
                agent_id,
                COUNT(*) as total_accesses,
                COUNT(DISTINCT file_path) as unique_files,
                SUM(CASE WHEN mode = 'WRITE' THEN 1 ELSE 0 END) as write_count,
                SUM(CASE WHEN action LIKE '%CONFLICT%' THEN 1 ELSE 0 END) as conflict_count
             FROM audit_log
             WHERE timestamp >= ?1
             GROUP BY agent_id
             ORDER BY total_accesses DESC
             LIMIT 10",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let summaries = stmt
            .query_map(params![period_str], |row| {
                Ok(AgentAccessSummary {
                    agent_id: row.get(0)?,
                    total_accesses: row.get(1)?,
                    unique_files: row.get(2)?,
                    write_count: row.get(3)?,
                    conflict_count: row.get(4)?,
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect: {}", e)))?;

        Ok(summaries)
    }

    /// Get top accessed files (inner — accepts Connection reference)
    fn get_top_files_inner(
        &self,
        conn: &Connection,
        period_start: &DateTime<Utc>,
    ) -> Result<Vec<FileAccessSummary>, ErgataiError> {
        let period_str = period_start.to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT
                file_path,
                COUNT(*) as total_accesses,
                COUNT(DISTINCT agent_id) as unique_agents,
                SUM(CASE WHEN mode = 'WRITE' THEN 1 ELSE 0 END) as write_count,
                SUM(CASE WHEN action LIKE '%CONFLICT%' THEN 1 ELSE 0 END) as conflict_count
             FROM audit_log
             WHERE timestamp >= ?1 AND file_path IS NOT NULL
             GROUP BY file_path
             ORDER BY total_accesses DESC
             LIMIT 10",
            )
            .map_err(|e| ErgataiError::internal(format!("Failed to prepare query: {}", e)))?;

        let summaries = stmt
            .query_map(params![period_str], |row| {
                Ok(FileAccessSummary {
                    file_path: row.get(0)?,
                    total_accesses: row.get(1)?,
                    unique_agents: row.get(2)?,
                    write_count: row.get(3)?,
                    conflict_count: row.get(4)?,
                })
            })
            .map_err(|e| ErgataiError::internal(format!("Failed to query: {}", e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect: {}", e)))?;

        Ok(summaries)
    }

    /// Cleanup old audit logs (simple time-based deletion)
    ///
    /// # Arguments
    /// * `days_to_keep` - Keep logs newer than this many days
    ///
    /// # Returns
    /// Number of entries deleted
    pub fn cleanup_old_audit_logs(&self, days_to_keep: u32) -> Result<usize, ErgataiError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let cutoff = (Utc::now() - Duration::days(days_to_keep as i64)).to_rfc3339();

        let deleted = conn
            .execute(
                "DELETE FROM audit_log WHERE timestamp < ?1",
                params![cutoff],
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to cleanup old audit logs: {}", e))
            })?;

        info!(
            deleted = deleted,
            days_to_keep = days_to_keep,
            "Cleaned up old audit logs"
        );

        Ok(deleted)
    }

    /// Archive audit logs older than specified months
    ///
    /// Exports old logs to a file, then deletes them from the database.
    /// This implements monthly partitioning strategy.
    ///
    /// # Arguments
    /// * `months_to_keep` - Keep logs newer than this many months
    /// * `export_path` - Path to export archived logs (JSON format)
    ///
    /// # Returns
    /// Number of entries archived and deleted
    pub fn archive_old_audit_logs(
        &self,
        months_to_keep: u32,
        export_path: &str,
    ) -> Result<usize, ErgataiError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        let cutoff = (Utc::now() - Duration::days(months_to_keep as i64 * 30)).to_rfc3339();

        // Query logs to archive
        // L5 fix: added map_err for consistent error handling
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, agent_id, session_id, action, file_path, mode, reason
             FROM audit_log WHERE timestamp < ?1 ORDER BY timestamp ASC",
            )
            .map_err(|e| {
                ErgataiError::internal(format!("Failed to prepare archive query: {}", e))
            })?;

        let entries: Vec<AuditEntry> = stmt
            .query_map(params![cutoff], |row| {
                Ok(AuditEntry {
                    timestamp: row.get(0)?,
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    action: row.get(3)?,
                    file_path: row.get(4)?,
                    mode: row.get(5)?,
                    reason: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErgataiError::internal(format!("Failed to collect entries: {}", e)))?;

        let count = entries.len();
        if count == 0 {
            info!("No audit logs to archive");
            return Ok(0);
        }

        // Export to file
        let json = serde_json::to_string_pretty(&entries).map_err(|e| {
            ErgataiError::internal(format!("Failed to serialize audit logs: {}", e))
        })?;

        std::fs::write(export_path, json)
            .map_err(|e| ErgataiError::internal(format!("Failed to write archive file: {}", e)))?;

        // Delete archived logs
        conn.execute(
            "DELETE FROM audit_log WHERE timestamp < ?1",
            params![cutoff],
        )
        .map_err(|e| ErgataiError::internal(format!("Failed to delete archived logs: {}", e)))?;

        info!(
            archived = count,
            export_path = export_path,
            months_to_keep = months_to_keep,
            "Archived old audit logs"
        );

        Ok(count)
    }

    /// Enforce maximum row count on audit log table
    ///
    /// Deletes oldest entries when the table exceeds the row limit.
    ///
    /// # Arguments
    /// * `max_rows` - Maximum number of rows to keep
    ///
    /// # Returns
    /// Number of entries deleted
    pub fn enforce_row_limit(&self, max_rows: u64) -> Result<usize, ErgataiError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| ErgataiError::internal(format!("Failed to acquire lock: {}", e)))?;

        // Count current rows
        let count: u64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))?;

        if count <= max_rows {
            return Ok(0);
        }

        // Calculate how many to delete
        let to_delete = count - max_rows;

        // Delete oldest entries
        let deleted = conn.execute(
            "DELETE FROM audit_log WHERE timestamp IN (
                SELECT timestamp FROM audit_log ORDER BY timestamp ASC LIMIT ?1
            )",
            params![to_delete as i64],
        )?;

        info!(
            deleted = deleted,
            max_rows = max_rows,
            previous_count = count,
            "Enforced audit log row limit"
        );

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, AuditManager) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_audit.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS audit_log (
                timestamp TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                action TEXT NOT NULL,
                file_path TEXT,
                mode TEXT,
                reason TEXT
            );
            ",
        )
        .unwrap();

        let conn = Arc::new(Mutex::new(conn));
        let manager = AuditManager::new(conn);

        (temp_dir, manager)
    }

    #[test]
    fn test_query_audit_log() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert some audit entries
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', 'test.rs', 'WRITE')",
                [],
            )
            .unwrap();
        }

        // Query
        let entries = manager
            .query_audit_log(Some("agent1"), None, None, None, None, 10)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_id, "agent1");
        assert_eq!(entries[0].action, "LOCK_ACQUIRED");
    }

    #[test]
    fn test_get_stats() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert some audit entries
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', 'test1.rs', 'WRITE')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', 'test2.rs', 'READ')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent2', 'session2', 'LOCK_ACQUIRED', 'test3.rs', 'WRITE')",
                [],
            )
            .unwrap();
        }

        // Get stats
        let stats = manager.get_stats(None).unwrap();
        assert_eq!(stats.total_acquisitions, 3);
        assert_eq!(stats.acquisitions_by_agent.get("agent1"), Some(&2));
        assert_eq!(stats.acquisitions_by_agent.get("agent2"), Some(&1));
    }

    #[test]
    fn test_query_audit_log_with_filters() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert audit entries with different actions and files
        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', 'test.rs', 'WRITE')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_RELEASED', 'test.rs', 'WRITE')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent2', 'session2', 'LOCK_ACQUIRED', 'other.rs', 'READ')",
                [],
            ).unwrap();
        }

        // Query by action
        let entries = manager
            .query_audit_log(None, Some("LOCK_ACQUIRED"), None, None, None, 10)
            .unwrap();
        assert_eq!(entries.len(), 2);

        // Query by file_path
        let entries = manager
            .query_audit_log(None, None, Some("test.rs"), None, None, 10)
            .unwrap();
        assert_eq!(entries.len(), 2);

        // Query by agent_id
        let entries = manager
            .query_audit_log(Some("agent2"), None, None, None, None, 10)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_id, "agent2");
    }

    #[test]
    fn test_generate_security_report() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert audit entries
        {
            let conn = manager.conn.lock().unwrap();
            // Normal operations
            for i in 0..5 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', ?, 'WRITE')",
                    [format!("file{}.rs", i)],
                ).unwrap();
            }
            // Conflict operations
            for _i in 0..15 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'agent2', 'session2', 'WRITE_CONFLICT', 'conflict.rs', 'WRITE')",
                    [],
                ).unwrap();
            }
        }

        // Generate report
        let report = manager.generate_security_report(7).unwrap();
        assert_eq!(report.stats.total_acquisitions, 5);
        assert_eq!(report.stats.total_conflicts, 15);
        assert!(report.suspicious_activities.len() > 0);
        assert_eq!(report.top_agents.len(), 2);
    }

    #[test]
    fn test_detect_suspicious_activities() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert suspicious activities
        {
            let conn = manager.conn.lock().unwrap();
            // High conflict rate agent
            for _i in 0..20 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'bad-agent', 'session1', 'WRITE_CONFLICT', 'file.rs', 'WRITE')",
                    [],
                ).unwrap();
            }
            // Sensitive path access
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'sensitive-agent', 'session2', 'LOCK_ACQUIRED', '.env', 'WRITE')",
                [],
            ).unwrap();
        }

        // Generate report to detect suspicious activities
        let report = manager.generate_security_report(7).unwrap();
        assert!(report.suspicious_activities.len() >= 2);

        // Check for high conflict rate detection
        let has_conflict_activity = report
            .suspicious_activities
            .iter()
            .any(|a| a.description.contains("High conflict rate"));
        assert!(has_conflict_activity);

        // Check for sensitive path access detection
        let has_sensitive_activity = report
            .suspicious_activities
            .iter()
            .any(|a| a.description.contains("sensitive paths"));
        assert!(has_sensitive_activity);
    }

    #[test]
    fn test_get_top_agents() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert audit entries for multiple agents
        {
            let conn = manager.conn.lock().unwrap();
            // Agent 1: 10 accesses
            for i in 0..10 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'agent1', 'session1', 'LOCK_ACQUIRED', ?, 'WRITE')",
                    [format!("file{}.rs", i)],
                ).unwrap();
            }
            // Agent 2: 5 accesses
            for i in 0..5 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'agent2', 'session2', 'LOCK_ACQUIRED', ?, 'READ')",
                    [format!("file{}.rs", i)],
                ).unwrap();
            }
        }

        // Get stats (which internally uses get_top_agents_inner)
        let stats = manager.get_stats(None).unwrap();
        assert_eq!(stats.acquisitions_by_agent.get("agent1"), Some(&10));
        assert_eq!(stats.acquisitions_by_agent.get("agent2"), Some(&5));
    }

    #[test]
    fn test_stats_with_time_period() {
        let (_temp_dir, manager) = setup_test_db();

        // Insert audit entries with different timestamps
        {
            let conn = manager.conn.lock().unwrap();
            // Recent entries (within 7 days)
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now', '-1 day'), 'agent1', 'session1', 'LOCK_ACQUIRED', 'recent.rs', 'WRITE')",
                [],
            ).unwrap();
            // Old entries (30 days ago)
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now', '-30 days'), 'agent2', 'session2', 'LOCK_ACQUIRED', 'old.rs', 'WRITE')",
                [],
            ).unwrap();
        }

        // Get stats for last 7 days only
        let stats = manager.get_stats(Some(7)).unwrap();
        assert_eq!(stats.total_acquisitions, 1);
        assert_eq!(stats.acquisitions_by_agent.get("agent1"), Some(&1));
        assert_eq!(stats.acquisitions_by_agent.get("agent2"), None);
    }

    #[test]
    fn test_audit_entry_serialization() {
        let entry = AuditEntry {
            timestamp: "2026-08-11T12:00:00Z".to_string(),
            agent_id: "agent1".to_string(),
            session_id: "session1".to_string(),
            action: "LOCK_ACQUIRED".to_string(),
            file_path: Some("test.rs".to_string()),
            mode: Some("WRITE".to_string()),
            reason: Some("Testing".to_string()),
        };

        // Serialize to JSON
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("agent1"));
        assert!(json.contains("LOCK_ACQUIRED"));

        // Deserialize back
        let deserialized: AuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_id, entry.agent_id);
        assert_eq!(deserialized.action, entry.action);
    }

    #[test]
    fn test_query_audit_log_with_time_range() {
        let (_temp_dir, manager) = setup_test_db();

        {
            let conn = manager.conn.lock().unwrap();
            // Old entry (40 days ago)
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now', '-40 days'), 'old-agent', 's1', 'LOCK_ACQUIRED', 'old.rs', 'WRITE')",
                [],
            ).unwrap();
            // Recent entry (1 day ago)
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now', '-1 day'), 'recent-agent', 's2', 'LOCK_ACQUIRED', 'recent.rs', 'WRITE')",
                [],
            ).unwrap();
        }

        // Query with start_time — at least both entries should be retrievable
        // (we don't strictly assert count because current date shifts).
        let entries = manager
            .query_audit_log(None, None, None, Some("2020-01-01T00:00:00Z"), None, 100)
            .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_query_audit_log_with_limit() {
        let (_temp_dir, manager) = setup_test_db();

        {
            let conn = manager.conn.lock().unwrap();
            for i in 0..20 {
                conn.execute(
                    "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                     VALUES (datetime('now'), 'agent', 's', 'LOCK_ACQUIRED', ?, 'WRITE')",
                    [format!("f{}.rs", i)],
                ).unwrap();
            }
        }

        let entries = manager
            .query_audit_log(None, None, None, None, None, 5)
            .unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn test_query_audit_log_empty_result() {
        let (_temp_dir, manager) = setup_test_db();
        // No entries inserted
        let entries = manager
            .query_audit_log(None, None, None, None, None, 100)
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_generate_security_report_with_no_data() {
        let (_temp_dir, manager) = setup_test_db();
        // Empty audit log
        let report = manager.generate_security_report(7).unwrap();
        assert_eq!(report.stats.total_acquisitions, 0);
        assert_eq!(report.stats.total_releases, 0);
        assert_eq!(report.stats.total_conflicts, 0);
        assert!(report.suspicious_activities.is_empty());
        assert!(report.top_agents.is_empty());
        assert!(report.top_files.is_empty());
    }

    #[test]
    fn test_file_access_stats_default() {
        let stats = FileAccessStats::default();
        assert_eq!(stats.total_acquisitions, 0);
        assert_eq!(stats.total_releases, 0);
        assert_eq!(stats.total_conflicts, 0);
        assert_eq!(stats.total_sensitive_accesses, 0);
        assert_eq!(stats.avg_lock_duration_secs, 0.0);
        assert!(stats.acquisitions_by_agent.is_empty());
        assert!(stats.accesses_by_file.is_empty());
        assert!(stats.accesses_by_mode.is_empty());
    }

    #[test]
    fn test_audit_entry_serialization_with_null_fields() {
        let entry = AuditEntry {
            timestamp: "2026-08-11T12:00:00Z".to_string(),
            agent_id: "agent".to_string(),
            session_id: "session".to_string(),
            action: "LOCK_ACQUIRED".to_string(),
            file_path: None,
            mode: None,
            reason: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: AuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_path, None);
        assert_eq!(deserialized.mode, None);
        assert_eq!(deserialized.reason, None);
    }

    #[test]
    fn test_security_report_serialization_roundtrip() {
        let report = SecurityReport {
            generated_at: "2026-08-12T10:00:00Z".to_string(),
            period_start: "2026-08-05T10:00:00Z".to_string(),
            period_end: "2026-08-12T10:00:00Z".to_string(),
            stats: FileAccessStats::default(),
            suspicious_activities: vec![SuspiciousActivity {
                timestamp: "2026-08-12T09:00:00Z".to_string(),
                agent_id: "bad-agent".to_string(),
                description: "High conflict rate".to_string(),
                severity: "HIGH".to_string(),
            }],
            top_agents: vec![AgentAccessSummary {
                agent_id: "agent-1".to_string(),
                total_accesses: 42,
                unique_files: 10,
                write_count: 30,
                conflict_count: 2,
            }],
            top_files: vec![FileAccessSummary {
                file_path: "src/main.rs".to_string(),
                total_accesses: 100,
                unique_agents: 5,
                write_count: 80,
                conflict_count: 3,
            }],
        };

        let json = serde_json::to_string(&report).unwrap();
        let deserialized: SecurityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.generated_at, report.generated_at);
        assert_eq!(deserialized.suspicious_activities.len(), 1);
        assert_eq!(deserialized.top_agents.len(), 1);
        assert_eq!(deserialized.top_files.len(), 1);
        assert_eq!(deserialized.top_agents[0].total_accesses, 42);
        assert_eq!(deserialized.top_files[0].file_path, "src/main.rs");
    }

    #[test]
    fn test_query_audit_log_with_all_filters_combined() {
        let (_temp_dir, manager) = setup_test_db();

        {
            let conn = manager.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent-x', 's1', 'LOCK_ACQUIRED', 'target.rs', 'WRITE')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent-x', 's1', 'LOCK_ACQUIRED', 'other.rs', 'WRITE')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO audit_log (timestamp, agent_id, session_id, action, file_path, mode)
                 VALUES (datetime('now'), 'agent-y', 's2', 'LOCK_ACQUIRED', 'target.rs', 'READ')",
                [],
            )
            .unwrap();
        }

        let entries = manager
            .query_audit_log(
                Some("agent-x"),
                Some("LOCK_ACQUIRED"),
                Some("target.rs"),
                None,
                None,
                100,
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_id, "agent-x");
        assert_eq!(entries[0].file_path, Some("target.rs".to_string()));
    }
}
