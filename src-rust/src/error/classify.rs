// Error classification and conversion logic
// Handles anyhow::Error → ErgataiError and ErgataiError → NapiError conversions

use napi::Error as NapiError;

use super::types::ErgataiError;

/// Convert ErgataiError to NAPI Error
impl From<ErgataiError> for NapiError {
    fn from(err: ErgataiError) -> Self {
        // Log the error before converting
        err.emit_log();

        // Format: [ERROR_CODE] Error message
        NapiError::from_reason(format!("[{}] {}", err.error_code(), err))
    }
}

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
    fn test_napi_conversion() {
        let err = ErgataiError::agent_timeout("test timeout");
        let napi_err: NapiError = err.into();
        assert!(napi_err
            .to_string()
            .contains("[ERR_AGENT_TIMEOUT] Agent timeout: test timeout"));
    }
}
