//! tmux binary locator.
//!
//! tmux is a system tool typically installed via the OS package manager.
//! Unlike rmux and nats-server, it is not bundled with ergatai — instead,
//! we locate it through multiple strategies:
//!
//! 1. `ERGATAI_TMUX_BINARY` environment variable override
//! 2. Bundled resources (for future download support)
//! 3. System PATH (`/usr/bin`, `/usr/local/bin`, Homebrew, etc.)
//!
//! If tmux is not found, [`find_tmux_binary`] returns an error with
//! platform-specific installation instructions.

use crate::finder::BinaryLocator;
use ergatai_error::{ErgataiError, ErgataiResult};
use std::path::PathBuf;
use std::sync::OnceLock;

static TMUX_LOCATOR: BinaryLocator = BinaryLocator {
    name: "tmux",
    env_override: Some("ERGATAI_TMUX_BINARY"),
    // tmux is not bundled by build.rs (yet), but the pattern is set so that
    // future bundled downloads are discovered automatically.
    resource_subdir_pattern: None,
};

/// Cached tmux binary path. Shared across all crates (ergatai-runtime, ergatai-collab)
/// so the BinaryLocator search runs at most once per process.
static TMUX_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Find tmux binary via BinaryLocator (env var → bundled → sibling → PATH).
///
/// This is the primary entry point for locating tmux. It follows the same
/// multi-layer strategy as [`crate::find_nats_binary`] and the rmux locator.
///
/// # Detection order
///
/// 1. `ERGATAI_TMUX_BINARY` env var
/// 2. Bundled resources (next to executable / in resources/)
/// 3. System `$PATH` (covers `/usr/bin`, `/usr/local/bin`, Homebrew, etc.)
///
/// # Errors
///
/// Returns [`ErgataiError::Internal`] with platform-specific install
/// instructions if tmux is not found.
pub fn find_tmux_binary() -> ErgataiResult<PathBuf> {
    TMUX_LOCATOR.find().map_err(|_| tmux_not_found_error())
}

/// Find tmux binary and cache the result for the lifetime of the process.
///
/// Subsequent calls return the cached path without re-running the BinaryLocator
/// search. If two threads call this simultaneously, both may compute the path
/// but only one wins — the result is identical so the race is harmless.
///
/// Use this in hot paths (e.g., every `run_tmux_cmd` call) to avoid repeated
/// multi-layer searches. For one-shot checks, [`find_tmux_binary`] is fine.
pub fn find_tmux_binary_cached() -> ErgataiResult<&'static PathBuf> {
    if let Some(path) = TMUX_PATH.get() {
        return Ok(path);
    }
    let path = find_tmux_binary()?;
    Ok(TMUX_PATH.get_or_init(|| path))
}

/// Check if tmux is available on this system.
///
/// Returns `true` if [`find_tmux_binary`] resolves to an existing binary.
/// Does not verify that the binary actually runs (use `tmux -V` for that).
pub fn is_tmux_available() -> bool {
    find_tmux_binary().is_ok()
}

/// Build an error message with platform-specific tmux installation instructions.
fn tmux_not_found_error() -> ErgataiError {
    let instructions = if cfg!(target_os = "macos") {
        "Install tmux: brew install tmux"
    } else if cfg!(target_os = "windows") {
        "Install tmux via MSYS2: pacman -S tmux"
    } else if cfg!(target_os = "linux") {
        "Install tmux: apt install tmux / dnf install tmux / pacman -S tmux"
    } else {
        "Install tmux via your system package manager"
    };

    ErgataiError::internal(format!(
        "tmux binary not found. {}\n\
         Or set ERGATAI_TMUX_BINARY=/path/to/tmux to use a custom location.",
        instructions
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_tmux_available_returns_bool() {
        // Just verifies the function doesn't panic; result depends on environment.
        let _ = is_tmux_available();
    }

    #[test]
    fn test_tmux_not_found_error_mentions_install() {
        let err = tmux_not_found_error();
        let msg = err.to_string();
        assert!(
            msg.contains("tmux"),
            "error should mention tmux: {}",
            msg
        );
        assert!(
            msg.contains("ERGATAI_TMUX_BINARY"),
            "error should mention env var: {}",
            msg
        );
    }
}
