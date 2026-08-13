// Unified error handling for Ergatai
// Provides structured error types with proper categorization and logging

use napi::Error as NapiError;
use thiserror::Error;

/// Main error type for Ergatai
/// All errors should be converted to this type before being sent to NAPI
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

    /// Agent initialization failed
    #[error("Agent initialization failed: {0}")]
    AgentInitFailed(String),

    /// Agent timeout (idle or hard deadline)
    #[error("Agent timeout: {0}")]
    AgentTimeout(String),

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

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Database error
    #[error("Database error: {0}")]
    DatabaseError(String),

    // ===== Network Errors =====
    /// Network connection failed
    #[error("Network error: {0}")]
    NetworkError(String),

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

    // ===== Internal Errors =====
    /// Unexpected internal error
    #[error("Internal error: {0}")]
    InternalError(String),

    /// Channel communication error
    #[error("Channel error: {0}")]
    ChannelError(String),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

impl ErgataiError {
    /// Get error code for frontend consumption
    pub fn error_code(&self) -> &'static str {
        match self {
            // User errors
            ErgataiError::InvalidArgument(_) => "ERR_INVALID_ARG",
            ErgataiError::AgentNotFound(_) => "ERR_AGENT_NOT_FOUND",
            ErgataiError::SessionNotFound(_) => "ERR_SESSION_NOT_FOUND",
            ErgataiError::PermissionDenied(_) => "ERR_PERMISSION_DENIED",

            // Agent errors
            ErgataiError::AgentSpawnFailed(_) => "ERR_AGENT_SPAWN_FAILED",
            ErgataiError::AgentInitFailed(_) => "ERR_AGENT_INIT_FAILED",
            ErgataiError::AgentTimeout(_) => "ERR_AGENT_TIMEOUT",
            ErgataiError::AgentProtocolError(_) => "ERR_AGENT_PROTOCOL",
            ErgataiError::AgentProcessDied(_) => "ERR_AGENT_PROCESS_DIED",

            // System errors
            ErgataiError::IoError(_) => "ERR_IO",
            ErgataiError::ConfigError(_) => "ERR_CONFIG",
            ErgataiError::DatabaseError(_) => "ERR_DATABASE",

            // Network errors
            ErgataiError::NetworkError(_) => "ERR_NETWORK",
            ErgataiError::NatsError(_) => "ERR_NATS",

            // Business errors
            ErgataiError::SessionAlreadyExists(_) => "ERR_SESSION_EXISTS",
            ErgataiError::InvalidSessionState(_) => "ERR_INVALID_SESSION_STATE",

            // Internal errors
            ErgataiError::InternalError(_) => "ERR_INTERNAL",
            ErgataiError::ChannelError(_) => "ERR_CHANNEL",
            ErgataiError::JsonError(_) => "ERR_JSON",
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
            ErgataiError::AgentTimeout(_)
                | ErgataiError::NetworkError(_)
                | ErgataiError::NatsError(_)
                | ErgataiError::IoError(_)
        )
    }

    /// Log the error with appropriate level
    pub fn log(&self) {
        if self.is_user_error() {
            tracing::warn!(
                error_code = self.error_code(),
                error_message = %self,
                "User error occurred"
            );
        } else if self.is_transient() {
            tracing::info!(
                error_code = self.error_code(),
                error_message = %self,
                "Transient error occurred (may retry)"
            );
        } else {
            tracing::error!(
                error_code = self.error_code(),
                error_message = %self,
                "System error occurred"
            );
        }
    }
}

/// Convert ErgataiError to NAPI Error
impl From<ErgataiError> for NapiError {
    fn from(err: ErgataiError) -> Self {
        // Log the error before converting
        err.log();

        // Format: [ERROR_CODE] Error message
        NapiError::from_reason(format!("[{}] {}", err.error_code(), err))
    }
}

/// Convert anyhow::Error to ErgataiError
/// Traverses the full error chain to find known error types, then falls back to
/// string-based classification of the full chain message.
///
/// Classification strategy: score each category by how many of its "signals"
/// appear in the message. The highest-scoring category wins. This avoids the
/// order-dependent bugs of a simple if/else chain (e.g. "Config file not found"
/// matching "not found" before "config").
impl From<anyhow::Error> for ErgataiError {
    fn from(err: anyhow::Error) -> Self {
        // First, traverse the error chain to find known error types.
        // This handles cases where io::Error or serde_json::Error is wrapped with .context().
        for cause in err.chain() {
            if let Some(e) = cause.downcast_ref::<std::io::Error>() {
                return ErgataiError::IoError(std::io::Error::new(e.kind(), e.to_string()));
            }
            if cause.downcast_ref::<serde_json::Error>().is_some() {
                return ErgataiError::JsonError(serde_json::from_str::<serde_json::Value>("invalid").unwrap_err());
            }
        }

        // Fall back to string-based classification using the full error chain message.
        let msg = err.to_string();
        let msg_lower = msg.to_lowercase();

        // (category, score) — each matching signal adds 1 point.
        let mut scores: Vec<(ErgataiErrorClass, i32)> = Vec::new();

        let mut add = |class: ErgataiErrorClass, signal: bool| {
            if signal {
                // Find or insert
                if let Some(entry) = scores.iter_mut().find(|(c, _)| *c == class) {
                    entry.1 += 1;
                } else {
                    scores.push((class, 1));
                }
            }
        };

        // Agent-timeout: "timeout" alone is a strong signal
        add(ErgataiErrorClass::AgentTimeout, msg_lower.contains("timeout"));

        // Agent-not-found: requires both "agent" AND "not found"
        add(
            ErgataiErrorClass::AgentNotFound,
            msg_lower.contains("agent") && msg_lower.contains("not found"),
        );

        // Session-not-found: requires both "session" AND "not found"
        add(
            ErgataiErrorClass::SessionNotFound,
            msg_lower.contains("session") && msg_lower.contains("not found"),
        );

        // Permission-denied: "permission" is unambiguous
        add(ErgataiErrorClass::PermissionDenied, msg_lower.contains("permission"));

        // Spawn-failed: prefer "spawn" (specific) over bare "process" (noisy)
        add(ErgataiErrorClass::AgentSpawnFailed, msg_lower.contains("spawn"));

        // Init-failed: "init" OR "initialization"
        add(
            ErgataiErrorClass::AgentInitFailed,
            msg_lower.contains("init") || msg_lower.contains("initialization"),
        );

        // Protocol: "protocol" or "json-rpc"
        add(
            ErgataiErrorClass::AgentProtocolError,
            msg_lower.contains("protocol") || msg_lower.contains("json-rpc"),
        );

        // Channel error: "channel" alone is strong
        add(ErgataiErrorClass::ChannelError, msg_lower.contains("channel"));

        // NATS: specific subsystem name
        add(ErgataiErrorClass::NatsError, msg_lower.contains("nats"));

        // Network: "network" or "connection"
        add(
            ErgataiErrorClass::NetworkError,
            msg_lower.contains("network") || msg_lower.contains("connection"),
        );

        // Config: "config" or "configuration"
        add(
            ErgataiErrorClass::ConfigError,
            msg_lower.contains("config") || msg_lower.contains("configuration"),
        );

        // Pick the highest-scoring category; ties broken by declaration order above
        // (first-added wins, since we use strictly-greater comparison).
        let mut best: Option<(ErgataiErrorClass, i32)> = None;
        for (class, score) in scores {
            if best.is_none() || score > best.as_ref().unwrap().1 {
                best = Some((class, score));
            }
        }
        let winner = best.map(|(class, _)| class);

        match winner {
            Some(ErgataiErrorClass::AgentTimeout) => ErgataiError::AgentTimeout(msg),
            Some(ErgataiErrorClass::AgentNotFound) => ErgataiError::AgentNotFound(msg),
            Some(ErgataiErrorClass::SessionNotFound) => ErgataiError::SessionNotFound(msg),
            Some(ErgataiErrorClass::PermissionDenied) => ErgataiError::PermissionDenied(msg),
            Some(ErgataiErrorClass::AgentSpawnFailed) => ErgataiError::AgentSpawnFailed(msg),
            Some(ErgataiErrorClass::AgentInitFailed) => ErgataiError::AgentInitFailed(msg),
            Some(ErgataiErrorClass::AgentProtocolError) => ErgataiError::AgentProtocolError(msg),
            Some(ErgataiErrorClass::ChannelError) => ErgataiError::ChannelError(msg),
            Some(ErgataiErrorClass::NatsError) => ErgataiError::NatsError(msg),
            Some(ErgataiErrorClass::NetworkError) => ErgataiError::NetworkError(msg),
            Some(ErgataiErrorClass::ConfigError) => ErgataiError::ConfigError(msg),
            None => ErgataiError::InternalError(msg),
        }
    }
}

/// Internal helper for score-based error classification. Not part of the public
/// error surface — just a discriminant used inside `From<anyhow::Error>`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ErgataiErrorClass {
    AgentTimeout,
    AgentNotFound,
    SessionNotFound,
    PermissionDenied,
    AgentSpawnFailed,
    AgentInitFailed,
    AgentProtocolError,
    ChannelError,
    NatsError,
    NetworkError,
    ConfigError,
}

/// Helper macro for error conversion with context
#[macro_export]
macro_rules! with_context {
    ($err:expr, $context:expr) => {
        $err.map_err(|e| {
            let context_msg = format!("{}: {}", $context, e);
            $crate::error::ErgataiError::from(anyhow::anyhow!(context_msg))
        })
    };
}

/// Result type alias using ErgataiError
pub type ErgataiResult<T> = Result<T, ErgataiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(
            ErgataiError::InvalidArgument("test".into()).error_code(),
            "ERR_INVALID_ARG"
        );
        assert_eq!(
            ErgataiError::AgentTimeout("test".into()).error_code(),
            "ERR_AGENT_TIMEOUT"
        );
    }

    #[test]
    fn test_anyhow_conversion() {
        let err = anyhow::anyhow!("Session creation timeout");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::AgentTimeout(_)));
    }

    #[test]
    fn test_user_error_detection() {
        assert!(ErgataiError::InvalidArgument("test".into()).is_user_error());
        assert!(ErgataiError::SessionNotFound("test".into()).is_user_error());
        assert!(!ErgataiError::InternalError("test".into()).is_user_error());
    }

    #[test]
    fn test_transient_error_detection() {
        assert!(ErgataiError::AgentTimeout("test".into()).is_transient());
        assert!(ErgataiError::NetworkError("test".into()).is_transient());
        assert!(!ErgataiError::InvalidArgument("test".into()).is_transient());
    }

    #[test]
    fn test_anyhow_conversion_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let anyhow_err = anyhow::Error::from(io_err);
        let ergatai_err: ErgataiError = anyhow_err.into();
        assert!(matches!(ergatai_err, ErgataiError::IoError(_)));
    }

    #[test]
    fn test_anyhow_conversion_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let anyhow_err = anyhow::Error::from(json_err);
        let ergatai_err: ErgataiError = anyhow_err.into();
        assert!(matches!(ergatai_err, ErgataiError::JsonError(_)));
    }

    #[test]
    fn test_anyhow_conversion_string_classification() {
        // Agent not found
        let err = anyhow::anyhow!("Agent not found: claude");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::AgentNotFound(_)));

        // Session not found
        let err = anyhow::anyhow!("Session not found: abc123");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::SessionNotFound(_)));

        // Spawn failed
        let err = anyhow::anyhow!("Failed to spawn process");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::AgentSpawnFailed(_)));

        // Init failed
        let err = anyhow::anyhow!("Initialization failed");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::AgentInitFailed(_)));

        // Protocol error
        let err = anyhow::anyhow!("JSON-RPC protocol violation");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::AgentProtocolError(_)));

        // Permission denied
        let err = anyhow::anyhow!("Permission denied for operation");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::PermissionDenied(_)));

        // Channel error
        let err = anyhow::anyhow!("Channel closed unexpectedly");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::ChannelError(_)));

        // Network error
        let err = anyhow::anyhow!("Network connection failed");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::NetworkError(_)));

        // NATS error
        let err = anyhow::anyhow!("NATS connection lost");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::NatsError(_)));

        // Config error
        let err = anyhow::anyhow!("Invalid config format");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::ConfigError(_)));

        // Default to internal error
        let err = anyhow::anyhow!("Something went wrong");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::InternalError(_)));
    }

    #[test]
    fn test_anyhow_classification_ambiguous_messages() {
        // "not found" + "config" should be ConfigError, not InternalError.
        // Under the old order-dependent logic, "not found" matched first and
        // fell through to InternalError because neither "agent" nor "session"
        // was present.
        let err = anyhow::anyhow!("Config file not found: /etc/app.toml");
        let ergatai_err: ErgataiError = err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::ConfigError(_)),
            "expected ConfigError, got {:?}",
            ergatai_err
        );

        // "process initialization failed" should be AgentInitFailed, not
        // AgentSpawnFailed — "init" is a stronger signal for init-failed than
        // the noisy word "process" is for spawn-failed.
        let err = anyhow::anyhow!("Process initialization failed");
        let ergatai_err: ErgataiError = err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::AgentInitFailed(_)),
            "expected AgentInitFailed, got {:?}",
            ergatai_err
        );
    }
}
