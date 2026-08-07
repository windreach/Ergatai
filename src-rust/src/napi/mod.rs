//! NAPI-RS binding layer.
//!
//! Each submodule exposes `#[napi]` functions that delegate to the business
//! logic in the parent modules (`crate::acp`, `crate::agent`,
//! `crate::cross_agent`, etc.). Keeping all NAPI glue in one place makes the
//! boundary between JS and Rust explicit and easy to audit.

pub mod acp;
pub mod agents;
pub mod dag;
pub mod mcp;
pub mod skills;
pub mod tasks;

use napi::Error as NapiError;

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
