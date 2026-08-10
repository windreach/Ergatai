//! NAPI-RS binding layer.
//!
//! Each submodule exposes `#[napi]` functions that delegate to the business
//! logic in the parent modules (`crate::acp`, `crate::agent`,
//! `crate::cross_agent`, etc.). Keeping all NAPI glue in one place makes the
//! boundary between JS and Rust explicit and easy to audit.

pub mod acp;
pub mod agents;
pub mod dag;
pub mod file_access;
pub mod mcp;
pub mod nats;
pub mod skills;
pub mod tasks;

use napi::Error as NapiError;
use napi_derive::napi;
use std::path::PathBuf;

use crate::error::ErgataiError;

/// Shared helpers used by all NAPI modules.
/// Initialize logging and the panic hook exactly once.
///
/// Cheap to call on every NAPI entry — internally uses `std::sync::Once`.
#[inline(always)]
pub fn guard() {
    crate::init_logging();
    crate::init_panic_hook();
}

/// Convert `ErgataiError` into a `napi::Error`.
#[inline(always)]
pub fn to_napi(e: ErgataiError) -> NapiError {
    e.into()
}

/// Set the resources directory path for bundled assets.
///
/// Called from TypeScript on app startup to tell Rust where to find
/// bundled resources like agent icons.
#[napi]
pub fn set_resources_path(path: String) {
    guard();
    crate::set_resources_path(PathBuf::from(path));
}
