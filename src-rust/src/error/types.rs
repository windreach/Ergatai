// Error type definitions for Ergatai
// Provides structured error types with fine-grained ConfigError variants

use std::path::PathBuf;
use thiserror::Error;

/// Boxed error type for source chains
type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

// ===== Error Codes =====

/// Type-safe error codes for frontend consumption.
/// Replaces bare string constants — compiler catches typos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // User errors (4xx)
    InvalidArg,
    AgentNotFound,
    SessionNotFound,
    PermissionDenied,

    // Agent errors
    AgentSpawnFailed,
    AgentInitFailed,
    AgentTimeout,
    AgentProtocol,
    AgentProcessDied,

    // System errors
    Io,
    ConfigDirNotFound,
    ConfigFileNotFound,
    ConfigReadFailed,
    ConfigParseFailed,
    ConfigValidationFailed,
    ConfigInvalidValue,
    Database,

    // Network errors
    Network,
    Nats,

    // Business errors
    SessionExists,
    InvalidSessionState,
    LockConflict,
    InvalidPath,
    NotFound,

    // Internal errors
    Internal,
    Channel,
    Json,
}

impl ErrorCode {
    /// Machine-readable code string for API responses
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidArg => "ERR_INVALID_ARG",
            Self::AgentNotFound => "ERR_AGENT_NOT_FOUND",
            Self::SessionNotFound => "ERR_SESSION_NOT_FOUND",
            Self::PermissionDenied => "ERR_PERMISSION_DENIED",

            Self::AgentSpawnFailed => "ERR_AGENT_SPAWN_FAILED",
            Self::AgentInitFailed => "ERR_AGENT_INIT_FAILED",
            Self::AgentTimeout => "ERR_AGENT_TIMEOUT",
            Self::AgentProtocol => "ERR_AGENT_PROTOCOL",
            Self::AgentProcessDied => "ERR_AGENT_PROCESS_DIED",

            Self::Io => "ERR_IO",
            Self::ConfigDirNotFound => "ERR_CONFIG_DIR_NOT_FOUND",
            Self::ConfigFileNotFound => "ERR_CONFIG_FILE_NOT_FOUND",
            Self::ConfigReadFailed => "ERR_CONFIG_READ_FAILED",
            Self::ConfigParseFailed => "ERR_CONFIG_PARSE_FAILED",
            Self::ConfigValidationFailed => "ERR_CONFIG_VALIDATION_FAILED",
            Self::ConfigInvalidValue => "ERR_CONFIG_INVALID_VALUE",
            Self::Database => "ERR_DATABASE",

            Self::Network => "ERR_NETWORK",
            Self::Nats => "ERR_NATS",

            Self::SessionExists => "ERR_SESSION_EXISTS",
            Self::InvalidSessionState => "ERR_INVALID_SESSION_STATE",
            Self::LockConflict => "ERR_LOCK_CONFLICT",
            Self::InvalidPath => "ERR_INVALID_PATH",
            Self::NotFound => "ERR_NOT_FOUND",

            Self::Internal => "ERR_INTERNAL",
            Self::Channel => "ERR_CHANNEL",
            Self::Json => "ERR_JSON",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ===== Main Error Type =====

/// Main error type for Ergatai
/// All errors should be converted to this type before being sent to NAPI
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum ErgataiError {
    // ===== User Errors (4xx) =====
    /// Invalid argument provided by user
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// Agent configuration not found
    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    /// Session not found
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    // ===== Agent Errors =====
    /// Failed to spawn agent process
    #[error("Agent spawn failed: {0}")]
    AgentSpawnFailed(String),

    /// Agent initialization failed — preserves source error chain
    #[error("Agent initialization failed: {message}")]
    AgentInitFailed { message: String, #[source] source: Option<BoxError> },

    /// Agent timeout — preserves source error chain (e.g., tokio::time::error::Elapsed)
    #[error("Agent timeout: {message}")]
    AgentTimeout { message: String, #[source] source: Option<BoxError> },

    /// Agent protocol error (JSON-RPC, ACP protocol violations)
    #[error("Agent protocol error: {0}")]
    AgentProtocolError(String),

    /// Agent process died unexpectedly
    #[error("Agent process died: {0}")]
    AgentProcessDied(String),

    // ===== System Errors =====
    /// IO error (file, network, process)
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Configuration error (fine-grained variants)
    #[error(transparent)]
    ConfigError(#[from] ConfigError),

    /// Database error
    #[error("Database error: {0}")]
    DatabaseError(String),

    // ===== Network Errors =====
    /// Network connection failed — preserves source error chain
    #[error("Network error: {message}")]
    NetworkError { message: String, #[source] source: Option<BoxError> },

    /// NATS connection error
    #[error("NATS error: {0}")]
    NatsError(String),

    // ===== Business Logic Errors =====
    /// Session already exists
    #[error("Session already exists: {0}")]
    SessionAlreadyExists(String),

    /// Invalid session state
    #[error("Invalid session state: {0}")]
    InvalidSessionState(String),

    /// File lock conflict (another agent holds the lock)
    #[error("Lock conflict: {0}")]
    LockConflict(String),

    /// File lock conflict with retry advice (livelock prevention)
    ///
    /// Includes structured retry information so the caller can back off
    /// and retry with exponential delay. After `max_retries` the caller
    /// should give up.
    #[error("Lock conflict on {file_path}: {message} (retry after {retry_after_ms}ms, attempt {retry_count}/{max_retries})")]
    LockConflictWithRetry {
        file_path: String,
        message: String,
        retry_after_ms: u64,
        retry_count: u32,
        max_retries: u32,
        priority_boosted: bool,
    },

    /// Invalid file path (traversal, escaping project root, etc.)
    #[error("Invalid path: {0}")]
    InvalidPath(String),

    /// Resource not found
    #[error("Not found: {0}")]
    NotFound(String),

    // ===== Internal Errors =====
    /// Unexpected internal error — preserves source error chain
    #[error("Internal error: {message}")]
    InternalError { message: String, #[source] source: Option<BoxError> },

    /// Channel communication error
    #[error("Channel error: {0}")]
    ChannelError(String),

    /// JSON serialization/deserialization error — preserves source error chain
    #[error("JSON error: {message}")]
    JsonError { message: String, #[source] source: Option<BoxError> },
}

// ===== Source-preserving From impls =====

impl From<serde_json::Error> for ErgataiError {
    fn from(err: serde_json::Error) -> Self {
        ErgataiError::JsonError {
            message: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<agent_client_protocol::Error> for ErgataiError {
    fn from(err: agent_client_protocol::Error) -> Self {
        ErgataiError::InternalError {
            message: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

// M6 fix: Add From<rusqlite::Error> for cleaner error handling in lock_manager
impl From<rusqlite::Error> for ErgataiError {
    fn from(err: rusqlite::Error) -> Self {
        ErgataiError::DatabaseError(err.to_string())
    }
}

// ===== Helper constructors =====

impl ErgataiError {
    /// Internal error without source (backward compat convenience)
    pub fn internal(message: impl Into<String>) -> Self {
        Self::InternalError { message: message.into(), source: None }
    }

    /// Internal error with source chain
    pub fn internal_with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::InternalError { message: message.into(), source: Some(Box::new(source)) }
    }

    /// Network error without source
    pub fn network(message: impl Into<String>) -> Self {
        Self::NetworkError { message: message.into(), source: None }
    }

    /// Network error with source chain
    pub fn network_with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::NetworkError { message: message.into(), source: Some(Box::new(source)) }
    }

    /// Agent timeout without source
    pub fn agent_timeout(message: impl Into<String>) -> Self {
        Self::AgentTimeout { message: message.into(), source: None }
    }

    /// Agent timeout with source chain
    pub fn agent_timeout_with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::AgentTimeout { message: message.into(), source: Some(Box::new(source)) }
    }

    /// Agent init failed without source
    pub fn agent_init_failed(message: impl Into<String>) -> Self {
        Self::AgentInitFailed { message: message.into(), source: None }
    }

    /// Agent init failed with source chain
    pub fn agent_init_failed_with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::AgentInitFailed { message: message.into(), source: Some(Box::new(source)) }
    }

    /// JSON error without source
    pub fn json(message: impl Into<String>) -> Self {
        Self::JsonError { message: message.into(), source: None }
    }

    /// JSON error with source chain
    pub fn json_with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::JsonError { message: message.into(), source: Some(Box::new(source)) }
    }
}

// ===== Error Classification =====

impl ErgataiError {
    /// Get type-safe error code for frontend consumption
    pub fn error_code(&self) -> ErrorCode {
        match self {
            // User errors
            ErgataiError::InvalidArgument(_) => ErrorCode::InvalidArg,
            ErgataiError::AgentNotFound(_) => ErrorCode::AgentNotFound,
            ErgataiError::SessionNotFound(_) => ErrorCode::SessionNotFound,
            ErgataiError::PermissionDenied(_) => ErrorCode::PermissionDenied,

            // Agent errors
            ErgataiError::AgentSpawnFailed(_) => ErrorCode::AgentSpawnFailed,
            ErgataiError::AgentInitFailed { .. } => ErrorCode::AgentInitFailed,
            ErgataiError::AgentTimeout { .. } => ErrorCode::AgentTimeout,
            ErgataiError::AgentProtocolError(_) => ErrorCode::AgentProtocol,
            ErgataiError::AgentProcessDied(_) => ErrorCode::AgentProcessDied,

            // System errors
            ErgataiError::IoError(_) => ErrorCode::Io,
            ErgataiError::ConfigError(e) => match e {
                ConfigError::DirectoryNotFound => ErrorCode::ConfigDirNotFound,
                ConfigError::FileNotFound { .. } => ErrorCode::ConfigFileNotFound,
                ConfigError::ReadFailed { .. } => ErrorCode::ConfigReadFailed,
                ConfigError::ParseFailed { .. } => ErrorCode::ConfigParseFailed,
                ConfigError::ValidationFailed { .. } => ErrorCode::ConfigValidationFailed,
                ConfigError::InvalidValue { .. } => ErrorCode::ConfigInvalidValue,
            },
            ErgataiError::DatabaseError(_) => ErrorCode::Database,

            // Network errors
            ErgataiError::NetworkError { .. } => ErrorCode::Network,
            ErgataiError::NatsError(_) => ErrorCode::Nats,

            // Business errors
            ErgataiError::SessionAlreadyExists(_) => ErrorCode::SessionExists,
            ErgataiError::InvalidSessionState(_) => ErrorCode::InvalidSessionState,
            ErgataiError::LockConflict(_) => ErrorCode::LockConflict,
            ErgataiError::LockConflictWithRetry { .. } => ErrorCode::LockConflict,
            ErgataiError::InvalidPath(_) => ErrorCode::InvalidPath,
            ErgataiError::NotFound(_) => ErrorCode::NotFound,

            // Internal errors
            ErgataiError::InternalError { .. } => ErrorCode::Internal,
            ErgataiError::ChannelError(_) => ErrorCode::Channel,
            ErgataiError::JsonError { .. } => ErrorCode::Json,
        }
    }

    /// Check if this is a user error (recoverable)
    pub fn is_user_error(&self) -> bool {
        matches!(
            self,
            ErgataiError::InvalidArgument(_)
                | ErgataiError::AgentNotFound(_)
                | ErgataiError::SessionNotFound(_)
                | ErgataiError::PermissionDenied(_)
        )
    }

    /// Check if this is a transient error (retryable)
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ErgataiError::AgentTimeout { .. }
                | ErgataiError::NetworkError { .. }
                | ErgataiError::NatsError(_)
                | ErgataiError::IoError(_)
        )
    }

    /// Log the error with appropriate level
    pub fn emit_log(&self) {
        if self.is_user_error() {
            tracing::warn!(
                error_code = %self.error_code(),
                error_message = %self,
                "User error occurred"
            );
        } else if self.is_transient() {
            tracing::info!(
                error_code = %self.error_code(),
                error_message = %self,
                "Transient error occurred (may retry)"
            );
        } else {
            tracing::error!(
                error_code = %self.error_code(),
                error_message = %self,
                "System error occurred"
            );
        }
    }
}

// ===== Fine-grained Config Errors =====

/// Fine-grained configuration error types
/// Replaces the previous ConfigError(String) to provide better error handling
#[non_exhaustive]
#[derive(Error, Debug)]
pub enum ConfigError {
    /// Config directory not found
    #[error("config directory not found")]
    DirectoryNotFound,

    /// Config file not found
    #[error("config file not found: {path}")]
    FileNotFound { path: PathBuf },

    /// Failed to read config file
    #[error("config read failed: {source}")]
    ReadFailed { source: std::io::Error },

    /// Failed to parse config file
    #[error("config parse failed: {source}")]
    ParseFailed { source: serde_json::Error },

    /// Config validation failed
    #[error("config validation failed: {reason}")]
    ValidationFailed { reason: String },

    /// Invalid config value
    #[error("invalid config value for '{key}': {reason}")]
    InvalidValue { key: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_enum() {
        let err = ErgataiError::InvalidArgument("test".into());
        assert_eq!(err.error_code(), ErrorCode::InvalidArg);
        assert_eq!(err.error_code().as_str(), "ERR_INVALID_ARG");
        assert_eq!(format!("{}", err.error_code()), "ERR_INVALID_ARG");
    }

    #[test]
    fn test_error_code_display() {
        assert_eq!(ErrorCode::AgentTimeout.as_str(), "ERR_AGENT_TIMEOUT");
        assert_eq!(ErrorCode::Json.as_str(), "ERR_JSON");
    }

    #[test]
    fn test_config_error_variants() {
        let err = ConfigError::FileNotFound {
            path: PathBuf::from("/etc/app.toml"),
        };
        assert_eq!(
            err.to_string(),
            "config file not found: /etc/app.toml"
        );

        let ergatai_err = ErgataiError::ConfigError(err);
        assert_eq!(ergatai_err.error_code(), ErrorCode::ConfigFileNotFound);
    }

    #[test]
    fn test_config_error_read_failed() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err = ConfigError::ReadFailed { source: io_err };
        assert!(err.to_string().contains("config read failed"));

        let ergatai_err = ErgataiError::ConfigError(err);
        assert_eq!(ergatai_err.error_code(), ErrorCode::ConfigReadFailed);
    }

    #[test]
    fn test_config_error_parse_failed() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err = ConfigError::ParseFailed { source: json_err };
        assert!(err.to_string().contains("config parse failed"));

        let ergatai_err = ErgataiError::ConfigError(err);
        assert_eq!(ergatai_err.error_code(), ErrorCode::ConfigParseFailed);
    }

    #[test]
    fn test_config_error_validation_failed() {
        let err = ConfigError::ValidationFailed {
            reason: "invalid timeout value".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "config validation failed: invalid timeout value"
        );

        let ergatai_err = ErgataiError::ConfigError(err);
        assert_eq!(ergatai_err.error_code(), ErrorCode::ConfigValidationFailed);
    }

    #[test]
    fn test_config_error_invalid_value() {
        let err = ConfigError::InvalidValue {
            key: "max_retries".to_string(),
            reason: "must be positive".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid config value for 'max_retries': must be positive"
        );

        let ergatai_err = ErgataiError::ConfigError(err);
        assert_eq!(ergatai_err.error_code(), ErrorCode::ConfigInvalidValue);
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(
            ErgataiError::InvalidArgument("test".into()).error_code(),
            ErrorCode::InvalidArg
        );
        assert_eq!(
            ErgataiError::agent_timeout("test").error_code(),
            ErrorCode::AgentTimeout
        );
    }

    #[test]
    fn test_user_error_detection() {
        assert!(ErgataiError::InvalidArgument("test".into()).is_user_error());
        assert!(ErgataiError::SessionNotFound("test".into()).is_user_error());
        assert!(!ErgataiError::internal("test").is_user_error());
    }

    #[test]
    fn test_transient_error_detection() {
        assert!(ErgataiError::agent_timeout("test").is_transient());
        assert!(ErgataiError::network("test").is_transient());
        assert!(!ErgataiError::InvalidArgument("test".into()).is_transient());
    }

    #[test]
    fn test_source_chain_preserved() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let err = ErgataiError::network_with_source("failed to connect", io_err);

        // Source chain accessible via std::error::Error::source()
        use std::error::Error;
        assert!(err.source().is_some());
        assert!(err.to_string().contains("failed to connect"));
    }

    #[test]
    fn test_json_from_preserves_source() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err: ErgataiError = json_err.into();

        use std::error::Error;
        assert!(err.source().is_some());
        assert_eq!(err.error_code(), ErrorCode::Json);
    }

    #[test]
    fn test_internal_helpers() {
        let err1 = ErgataiError::internal("simple");
        assert!(matches!(err1, ErgataiError::InternalError { ref message, source: None } if message == "simple"));

        let io_err = std::io::Error::other("boom");
        let err2 = ErgataiError::internal_with_source("wrapped", io_err);
        assert!(matches!(err2, ErgataiError::InternalError { ref message, source: Some(_) } if message == "wrapped"));
    }
}
