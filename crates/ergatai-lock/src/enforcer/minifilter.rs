//! Windows Minifilter backend (stub).
//!
//! This module provides a stub implementation for the Windows Filter Manager
//! minifilter driver. Full implementation requires:
//! - Kernel-mode driver (.sys) installed and loaded
//! - Altitude registered with Microsoft
//! - User-mode service communicating via filter port
//! - `winapi` crate with filter feature (feature-gated)

#![cfg(target_os = "windows")]

use async_trait::async_trait;
use std::path::PathBuf;

use super::backend::{EnforcementResult, EnforcerBackend, FileAccessEvent, PlatformHandle};

/// Windows Minifilter backend.
///
/// Uses the Filter Manager minifilter driver to intercept IRP_MJ_CREATE
/// (file open) operations. Communication with the kernel driver is via
/// FilterSendMessage/FilterGetMessage.
///
/// # Prerequisites
///
/// 1. Minifilter driver (.sys) installed and loaded
/// 2. Altitude registered with Microsoft
/// 3. User-mode service communicating via filter port
pub struct MinifilterBackend {
    project_root: PathBuf,
    // port_handle: HANDLE,
}

impl MinifilterBackend {
    /// Initialize the Minifilter backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Minifilter driver not installed
    /// - Cannot connect to filter port
    /// - Insufficient permissions
    pub fn new(
        _project_root: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // TODO: FilterConnectCommunicationPort, create message queue
        // This requires the minifilter driver to be installed and a communication port.
        Err("Minifilter backend not yet implemented. \
             Requires kernel-mode driver (.sys) with registered altitude."
            .into())
    }
}

#[async_trait]
impl EnforcerBackend for MinifilterBackend {
    fn name(&self) -> &'static str {
        "minifilter"
    }

    fn is_mandatory(&self) -> bool {
        true
    }

    async fn next_event(&self) -> Option<FileAccessEvent> {
        // TODO: FilterGetMessage() on communication port
        // Receives IRP_MJ_CREATE pre-operation callbacks from the driver
        None
    }

    async fn respond(
        &self,
        _handle: PlatformHandle,
        _result: EnforcementResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // TODO: FilterReplyMessage() with FLT_REPLY
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // TODO: CloseHandle(port_handle)
        Ok(())
    }
}
