//! Token data structures for file access control.
//!
//! Two-tier token system:
//! - SystemToken: proves agent is authorized to participate in multi-agent collaboration
//! - FileToken: grants specific file operation permissions (READ/WRITE)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenId(String);

impl TokenId {
    /// Generate a new random token ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Create from existing string.
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Get the string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TokenId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TokenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// File access mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileMode {
    /// Shared read access (multiple agents can read in parallel).
    Read,
    /// Exclusive write access (only one writer per file).
    Write,
    /// Admin access: can override any lock, can issue sub-tokens.
    /// Requires human approval in all modes.
    Admin,
}

impl std::fmt::Display for FileMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileMode::Read => write!(f, "READ"),
            FileMode::Write => write!(f, "WRITE"),
            FileMode::Admin => write!(f, "ADMIN"),
        }
    }
}

/// System token: proves agent is authorized to participate in multi-agent collaboration.
///
/// Issued by the system when an ACP session starts. Required in multi-agent mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemToken {
    /// Unique token identifier.
    pub id: TokenId,
    /// Agent identifier (e.g., "claude-code", "codex").
    pub agent_id: String,
    /// ACP session identifier.
    pub session_id: String,
    /// Project root directory (absolute path).
    pub project_root: String,
    /// When the token was issued.
    pub issued_at: DateTime<Utc>,
    /// When the token expires.
    pub expires_at: DateTime<Utc>,
    /// Heartbeat interval in seconds.
    pub heartbeat_interval_secs: u64,
    /// Last heartbeat timestamp.
    pub heartbeat_at: DateTime<Utc>,
    /// Token status.
    pub status: TokenStatus,
}

impl SystemToken {
    /// Create a new system token.
    pub fn new(
        agent_id: String,
        session_id: String,
        project_root: String,
        ttl_secs: u64,
        heartbeat_interval_secs: u64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: TokenId::new(),
            agent_id,
            session_id,
            project_root,
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_secs as i64),
            heartbeat_interval_secs,
            heartbeat_at: now,
            status: TokenStatus::Active,
        }
    }

    /// Check if the token is valid (not expired and active).
    pub fn is_valid(&self) -> bool {
        self.status == TokenStatus::Active && Utc::now() < self.expires_at
    }

    /// Update the heartbeat timestamp.
    pub fn update_heartbeat(&mut self) {
        self.heartbeat_at = Utc::now();
    }

    /// Check if heartbeat has timed out.
    pub fn is_heartbeat_timeout(&self, timeout_multiplier: u32) -> bool {
        let timeout = chrono::Duration::seconds(
            (self.heartbeat_interval_secs * timeout_multiplier as u64) as i64,
        );
        Utc::now() > self.heartbeat_at + timeout
    }
}

/// File access token: grants specific file operation permissions.
///
/// Bound to agentId + sessionId. Covers multiple files within scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileToken {
    /// Unique token identifier.
    pub id: TokenId,
    /// Agent identifier.
    pub agent_id: String,
    /// ACP session identifier.
    pub session_id: String,
    /// Reference to the system token.
    pub system_token_id: TokenId,
    /// Scope pattern (glob, e.g., "src/auth/**").
    pub scope: String,
    /// Access mode (READ or WRITE).
    pub mode: FileMode,
    /// Reason for the request.
    pub reason: Option<String>,
    /// Who approved this token ("system" or agent_id).
    pub approved_by: String,
    /// When the token was issued.
    pub issued_at: DateTime<Utc>,
    /// When the token expires.
    pub expires_at: DateTime<Utc>,
    /// Heartbeat interval in seconds.
    pub heartbeat_interval_secs: u64,
    /// Last heartbeat timestamp.
    pub heartbeat_at: DateTime<Utc>,
    /// Token status.
    pub status: TokenStatus,
    /// Task priority (1=low, 2=medium, 3=high). Used for conflict arbitration.
    pub priority: Option<u8>,
}

impl FileToken {
    /// Create a new file token.
    pub fn new(
        agent_id: String,
        session_id: String,
        system_token_id: TokenId,
        scope: String,
        mode: FileMode,
        reason: Option<String>,
        approved_by: String,
        ttl_secs: u64,
        heartbeat_interval_secs: u64,
    ) -> Self {
        Self::with_priority(
            agent_id,
            session_id,
            system_token_id,
            scope,
            mode,
            reason,
            approved_by,
            ttl_secs,
            heartbeat_interval_secs,
            None,
        )
    }

    /// Create a new file token with explicit priority.
    pub fn with_priority(
        agent_id: String,
        session_id: String,
        system_token_id: TokenId,
        scope: String,
        mode: FileMode,
        reason: Option<String>,
        approved_by: String,
        ttl_secs: u64,
        heartbeat_interval_secs: u64,
        priority: Option<u8>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: TokenId::new(),
            agent_id,
            session_id,
            system_token_id,
            scope,
            mode,
            reason,
            approved_by,
            issued_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_secs as i64),
            heartbeat_interval_secs,
            heartbeat_at: now,
            status: TokenStatus::Active,
            priority,
        }
    }

    /// Check if the token is valid (not expired and active).
    pub fn is_valid(&self) -> bool {
        self.status == TokenStatus::Active && Utc::now() < self.expires_at
    }

    /// Update the heartbeat timestamp.
    pub fn update_heartbeat(&mut self) {
        self.heartbeat_at = Utc::now();
    }

    /// Check if heartbeat has timed out.
    pub fn is_heartbeat_timeout(&self, timeout_multiplier: u32) -> bool {
        let timeout = chrono::Duration::seconds(
            (self.heartbeat_interval_secs * timeout_multiplier as u64) as i64,
        );
        Utc::now() > self.heartbeat_at + timeout
    }

    /// Check if a file path is within this token's scope.
    pub fn matches_path(&self, file_path: &str) -> bool {
        // Normalize path (see H2 fix)
        let normalized_path = match normalize_path_for_matching(file_path) {
            Some(p) => p,
            None => return false,
        };

        // Parse glob pattern (case-insensitive on Windows)
        // M8 fix: avoid unnecessary allocation on non-Windows
        #[cfg(windows)]
        let pattern_str = self.scope.to_lowercase();
        #[cfg(not(windows))]
        let pattern_str = &self.scope;

        #[cfg(windows)]
        let pattern_ref: &str = &pattern_str;
        #[cfg(not(windows))]
        let pattern_ref: &str = pattern_str;

        let pattern = match glob::Pattern::new(pattern_ref) {
            Ok(p) => p,
            Err(_) => return false,
        };

        pattern.matches(&normalized_path)
    }
}

/// Token status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TokenStatus {
    /// Token is active and valid.
    Active,
    /// Token is being upgraded (READ → WRITE).
    Upgrading,
    /// Token has expired.
    Expired,
    /// Token has been revoked.
    Revoked,
}

impl std::fmt::Display for TokenStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenStatus::Active => write!(f, "ACTIVE"),
            TokenStatus::Upgrading => write!(f, "UPGRADING"),
            TokenStatus::Expired => write!(f, "EXPIRED"),
            TokenStatus::Revoked => write!(f, "REVOKED"),
        }
    }
}

/// File lock record (Phase 5: for watchdog lock reclaim).
///
/// Represents a lock on a specific file, associated with a FileToken.
#[derive(Debug, Clone)]
pub struct FileLock {
    /// Unique lock identifier.
    pub id: String,
    /// File path (absolute).
    pub file_path: String,
    /// Agent identifier.
    pub agent_id: String,
    /// ACP session identifier.
    pub session_id: String,
    /// Access mode (READ or WRITE).
    pub mode: FileMode,
    /// Scope pattern (glob).
    pub scope: String,
    /// Reference to the system token.
    pub token_id: TokenId,
    /// Reason for the lock.
    pub reason: Option<String>,
    /// Who approved this lock.
    pub approved_by: String,
    /// When the lock was created.
    pub created_at: DateTime<Utc>,
    /// When the lock expires.
    pub expires_at: DateTime<Utc>,
    /// Heartbeat interval in seconds.
    pub heartbeat_interval_secs: u64,
    /// Last heartbeat timestamp.
    pub heartbeat_at: DateTime<Utc>,
    /// Lock status.
    pub status: TokenStatus,
}

/// Normalize a path for scope matching (H2 fix).
///
/// - Removes leading "./" if present
/// - Converts to lowercase on Windows
/// - Returns None if path contains ".." (security check)
fn normalize_path_for_matching(path: &str) -> Option<String> {
    // Reject paths containing ".." (path traversal attempt)
    if path.contains("..") {
        return None;
    }

    // Remove leading "./" if present
    let cleaned = path.strip_prefix("./").unwrap_or(path);

    // Convert to lowercase on Windows (case-insensitive matching)
    let normalized = if cfg!(windows) {
        cleaned.to_lowercase()
    } else {
        cleaned.to_string()
    };

    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_id_generation() {
        let id1 = TokenId::new();
        let id2 = TokenId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_system_token_validity() {
        let token = SystemToken::new(
            "test-agent".to_string(),
            "session-123".to_string(),
            "/project".to_string(),
            3600,
            30,
        );
        assert!(token.is_valid());
    }

    #[test]
    fn test_file_token_scope_matching() {
        let token = FileToken::new(
            "agent-1".to_string(),
            "session-1".to_string(),
            TokenId::new(),
            "src/auth/**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );

        assert!(token.matches_path("src/auth/login.ts"));
        assert!(token.matches_path("src/auth/jwt/verify.rs"));
        assert!(!token.matches_path("src/db/schema.ts"));
        assert!(!token.matches_path("README.md"));
    }

    #[test]
    fn test_path_normalization_rejects_traversal() {
        let token = FileToken::new(
            "agent-1".to_string(),
            "session-1".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );

        // Should reject path traversal attempts
        assert!(!token.matches_path("../etc/passwd"));
        assert!(!token.matches_path("src/../../etc/passwd"));
    }

    #[test]
    fn test_file_mode_display() {
        assert_eq!(FileMode::Read.to_string(), "READ");
        assert_eq!(FileMode::Write.to_string(), "WRITE");
    }

    #[test]
    fn test_file_mode_admin_display() {
        assert_eq!(FileMode::Admin.to_string(), "ADMIN");
    }

    #[test]
    fn test_token_status_display_all_variants() {
        assert_eq!(TokenStatus::Active.to_string(), "ACTIVE");
        assert_eq!(TokenStatus::Upgrading.to_string(), "UPGRADING");
        assert_eq!(TokenStatus::Expired.to_string(), "EXPIRED");
        assert_eq!(TokenStatus::Revoked.to_string(), "REVOKED");
    }

    #[test]
    fn test_token_id_from_string_and_as_str() {
        let id = TokenId::from_string("custom-id-123".to_string());
        assert_eq!(id.as_str(), "custom-id-123");
        assert_eq!(id.to_string(), "custom-id-123");
    }

    #[test]
    fn test_token_id_default_is_unique() {
        let id1: TokenId = Default::default();
        let id2: TokenId = Default::default();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_token_id_equality() {
        let a = TokenId::from_string("same".to_string());
        let b = TokenId::from_string("same".to_string());
        let c = TokenId::from_string("different".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_system_token_update_heartbeat() {
        let mut token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            3600,
            30,
        );
        let original_heartbeat = token.heartbeat_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        token.update_heartbeat();
        assert!(token.heartbeat_at > original_heartbeat);
    }

    #[test]
    fn test_system_token_is_heartbeat_timeout_not_timed_out() {
        let token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            3600,
            30,
        );
        // Fresh heartbeat → no timeout even with multiplier=1
        assert!(!token.is_heartbeat_timeout(1));
        assert!(!token.is_heartbeat_timeout(2));
    }

    #[test]
    fn test_system_token_is_heartbeat_timeout_when_stale() {
        let mut token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            3600,
            1, // 1-second heartbeat interval
        );
        // Manually set heartbeat far in the past
        token.heartbeat_at = Utc::now() - chrono::Duration::seconds(60);
        // With multiplier=2, timeout = 1*2 = 2 seconds. 60s old → timed out
        assert!(token.is_heartbeat_timeout(2));
    }

    #[test]
    fn test_system_token_invalid_when_zero_ttl() {
        let token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            0, // 0-second TTL → already expired
            30,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(!token.is_valid());
    }

    #[test]
    fn test_system_token_invalid_when_revoked() {
        let mut token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            3600,
            30,
        );
        token.status = TokenStatus::Revoked;
        assert!(!token.is_valid());
    }

    #[test]
    fn test_system_token_invalid_when_upgrading() {
        let mut token = SystemToken::new(
            "agent".to_string(),
            "session".to_string(),
            "/project".to_string(),
            3600,
            30,
        );
        token.status = TokenStatus::Upgrading;
        assert!(!token.is_valid());
    }

    #[test]
    fn test_file_token_is_valid() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(token.is_valid());
    }

    #[test]
    fn test_file_token_invalid_when_expired() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            0, // 0-second TTL
            15,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(!token.is_valid());
    }

    #[test]
    fn test_file_token_update_heartbeat() {
        let mut token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Write,
            Some("refactor".to_string()),
            "system".to_string(),
            3600,
            15,
        );
        let original = token.heartbeat_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        token.update_heartbeat();
        assert!(token.heartbeat_at > original);
    }

    #[test]
    fn test_file_token_is_heartbeat_timeout_when_stale() {
        let mut token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            1, // 1-second heartbeat
        );
        token.heartbeat_at = Utc::now() - chrono::Duration::seconds(60);
        assert!(token.is_heartbeat_timeout(2));
    }

    #[test]
    fn test_file_token_is_heartbeat_timeout_fresh() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            30,
        );
        assert!(!token.is_heartbeat_timeout(1));
    }

    #[test]
    fn test_matches_path_exact_file() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "src/main.rs".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(token.matches_path("src/main.rs"));
        assert!(!token.matches_path("src/lib.rs"));
        assert!(!token.matches_path("src/main.rs.bak"));
    }

    #[test]
    fn test_matches_path_single_star_glob() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "src/*.rs".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(token.matches_path("src/main.rs"));
        assert!(token.matches_path("src/lib.rs"));
        // NOTE: Rust's `glob::Pattern::matches` uses default MatchOptions which
        // has `require_literal_separator: false`, so `*` CAN cross `/`.
        // Use `**` for explicitly recursive matching; single `*` here also matches.
        assert!(token.matches_path("src/sub/mod.rs"));
        // But a prefix mismatch still fails
        assert!(!token.matches_path("tests/main.rs"));
    }

    #[test]
    fn test_matches_path_double_star_recursive() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "src/**/*.rs".to_string(),
            FileMode::Write,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(token.matches_path("src/main.rs"));
        assert!(token.matches_path("src/auth/login.rs"));
        assert!(token.matches_path("src/a/b/c/d.rs"));
        assert!(!token.matches_path("src/main.ts"));
        assert!(!token.matches_path("tests/test.rs"));
    }

    #[test]
    fn test_matches_path_leading_dot_slash_stripped() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "src/**/*.rs".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        // Leading "./" should be normalized away
        assert!(token.matches_path("./src/main.rs"));
        assert!(token.matches_path("./src/auth/login.rs"));
    }

    #[test]
    fn test_matches_path_invalid_glob_returns_false() {
        // An unclosed bracket is an invalid glob pattern
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "src/[unclosed".to_string(),
            FileMode::Read,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(!token.matches_path("src/anything.rs"));
    }

    #[test]
    fn test_matches_path_double_star_all_files() {
        let token = FileToken::new(
            "agent".to_string(),
            "session".to_string(),
            TokenId::new(),
            "**".to_string(),
            FileMode::Admin,
            None,
            "system".to_string(),
            3600,
            15,
        );
        assert!(token.matches_path("anything.txt"));
        assert!(token.matches_path("src/deep/path/file.rs"));
    }

    #[test]
    fn test_file_mode_serde_roundtrip() {
        let modes = vec![FileMode::Read, FileMode::Write, FileMode::Admin];
        for mode in modes {
            let json = serde_json::to_string(&mode).unwrap();
            let expected = match mode {
                FileMode::Read => "\"READ\"",
                FileMode::Write => "\"WRITE\"",
                FileMode::Admin => "\"ADMIN\"",
            };
            assert_eq!(json, expected);
            let deserialized: FileMode = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, mode);
        }
    }

    #[test]
    fn test_token_status_serde_roundtrip() {
        let statuses = vec![
            TokenStatus::Active,
            TokenStatus::Upgrading,
            TokenStatus::Expired,
            TokenStatus::Revoked,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: TokenStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, status);
        }
    }
}
