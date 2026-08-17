// Error classification and conversion logic
// Handles anyhow::Error → ErgataiError conversions

use super::types::ErgataiError;

/// Convert anyhow::Error to ErgataiError
/// Traverses the error chain to find known error types (io::Error, serde_json::Error),
/// then falls back to InternalError with the full error chain preserved.
///
/// Note: String-based classification has been removed as it was fragile and hard to maintain.
/// Callers should construct ErgataiError variants directly when the error type is known.
impl From<anyhow::Error> for ErgataiError {
    fn from(err: anyhow::Error) -> Self {
        // Traverse the error chain to find known error types.
        // This handles cases where io::Error or serde_json::Error is wrapped with .context().
        for cause in err.chain() {
            if let Some(e) = cause.downcast_ref::<std::io::Error>() {
                return ErgataiError::IoError(std::io::Error::new(e.kind(), e.to_string()));
            }
            if let Some(e) = cause.downcast_ref::<serde_json::Error>() {
                return ErgataiError::json(e.to_string());
            }
        }

        // TODO: Consider adding more chain downcast patterns if needed

        // Fallback: preserve the full error chain using alternate Display
        // Format: "top-level error: cause1: cause2: root cause"
        ErgataiError::internal(format!("{:#}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anyhow_conversion_unknown_error() {
        // Unknown error types fall back to InternalError with full chain preserved
        let err = anyhow::anyhow!("Session creation timeout");
        let ergatai_err: ErgataiError = err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::InternalError { .. }),
            "expected InternalError, got {:?}",
            ergatai_err
        );
        // Verify full chain is preserved
        assert!(ergatai_err.to_string().contains("Session creation timeout"));
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
        assert!(matches!(ergatai_err, ErgataiError::JsonError { .. }));
    }

    #[test]
    fn test_anyhow_conversion_io_error_with_context() {
        // io::Error wrapped with .context() should still be detected via chain traversal
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let anyhow_err = anyhow::Error::from(io_err).context("failed to read config file");
        let ergatai_err: ErgataiError = anyhow_err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::IoError(_)),
            "expected IoError, got {:?}",
            ergatai_err
        );
    }

    #[test]
    fn test_anyhow_conversion_io_error_preserves_kind() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let anyhow_err = anyhow::Error::from(io_err);
        let ergatai_err: ErgataiError = anyhow_err.into();
        if let ErgataiError::IoError(e) = ergatai_err {
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        } else {
            panic!("expected IoError");
        }
    }

    #[test]
    fn test_anyhow_conversion_serde_json_with_context() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let anyhow_err = anyhow::Error::from(json_err).context("failed to parse response");
        let ergatai_err: ErgataiError = anyhow_err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::JsonError { .. }),
            "expected JsonError, got {:?}",
            ergatai_err
        );
    }

    #[test]
    fn test_anyhow_conversion_fallback_preserves_full_chain() {
        // When no known error type is in the chain, the full chain should be preserved
        let err = anyhow::anyhow!("top error")
            .context("middle context")
            .context("outer context");
        let ergatai_err: ErgataiError = err.into();
        let msg = ergatai_err.to_string();
        // The alternate display format preserves the chain: "outer: middle: top"
        assert!(
            msg.contains("top error"),
            "should contain root cause: {}",
            msg
        );
        assert!(
            msg.contains("middle context"),
            "should contain middle context: {}",
            msg
        );
        assert!(
            msg.contains("outer context"),
            "should contain outer context: {}",
            msg
        );
    }

    #[test]
    fn test_anyhow_conversion_empty_message() {
        let err = anyhow::anyhow!("");
        let ergatai_err: ErgataiError = err.into();
        assert!(matches!(ergatai_err, ErgataiError::InternalError { .. }));
    }

    #[test]
    fn test_anyhow_conversion_io_error_deeply_nested() {
        // io::Error deeply nested in context chain
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let anyhow_err = anyhow::Error::from(io_err)
            .context("socket connect failed")
            .context("nats handshake failed")
            .context("event bus unavailable");
        let ergatai_err: ErgataiError = anyhow_err.into();
        assert!(
            matches!(ergatai_err, ErgataiError::IoError(_)),
            "should find io::Error through deep chain"
        );
    }

    #[test]
    fn test_anyhow_conversion_fallback_is_internal_error() {
        let err = anyhow::anyhow!("something went wrong");
        let ergatai_err: ErgataiError = err.into();
        match &ergatai_err {
            ErgataiError::InternalError { message, .. } => {
                assert!(message.contains("something went wrong"));
            }
            other => panic!("expected InternalError, got {:?}", other),
        }
    }
}
