//! macOS Endpoint Security backend (stub).
//!
//! This module provides a stub implementation for the Endpoint Security framework
//! on macOS. Full implementation requires:
//! - macOS 10.15+ system extension entitlement
//! - `endpoint-security` crate (feature-gated)
//! - System extension deployed and approved by user
//! - Full Disk Access permission
//! - Binary signed with `com.apple.developer.endpoint-security.client` entitlement

#![cfg(target_os = "macos")]

use async_trait::async_trait;
use std::path::PathBuf;

use super::backend::{EnforcementResult, EnforcerBackend, FileAccessEvent, PlatformHandle};

/// macOS Endpoint Security backend.
///
/// Uses the Endpoint Security framework to intercept file open operations.
/// Requires a system extension signed with the Endpoint Security entitlement.
///
/// # Prerequisites
///
/// 1. System extension deployed and approved by user in System Preferences
/// 2. Full Disk Access permission granted
/// 3. Binary signed with `com.apple.developer.endpoint-security.client` entitlement
pub struct EndpointSecurityBackend {
    project_root: PathBuf,
}

impl EndpointSecurityBackend {
    /// Initialize the Endpoint Security backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Not running on macOS 10.15+
    /// - System extension not installed or not approved
    /// - Insufficient permissions
    pub fn new(
        _project_root: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // TODO: Initialize ES client, subscribe to OPEN events
        // This requires the `endpoint-security` crate and proper entitlements.
        Err("Endpoint Security backend not yet implemented. \
             Requires system extension with ES entitlement."
            .into())
    }
}

#[async_trait]
impl EnforcerBackend for EndpointSecurityBackend {
    fn name(&self) -> &'static str {
        "endpoint-security"
    }

    fn is_mandatory(&self) -> bool {
        true
    }

    async fn next_event(&self) -> Option<FileAccessEvent> {
        // TODO: Receive ES auth events via channel
        // es_event_type_t::ES_EVENT_TYPE_AUTH_OPEN
        None
    }

    async fn respond(
        &self,
        _handle: PlatformHandle,
        _result: EnforcementResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // TODO: es_respond_auth_result()
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // TODO: es_destroy_client()
        Ok(())
    }
}
