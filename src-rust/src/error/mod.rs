// Error handling module for Ergatai
// Provides structured error types with fine-grained ConfigError variants

mod types;
mod classify;

// Re-export public API
pub use types::{ErgataiError, ConfigError, ErrorCode};

/// Result type alias using ErgataiError
pub type ErgataiResult<T> = Result<T, ErgataiError>;
