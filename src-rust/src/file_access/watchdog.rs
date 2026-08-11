//! Watchdog module for token expiration and heartbeat monitoring
//!
//! Phase 5: Implements background monitoring for:
//! - Token heartbeat timeout detection (3x interval)
//! - ACP disconnect detection → auto-reclaim locks
//! - Task-aware heartbeat (busy status + progressive timeout)
//! - Broadcast file.error events on crash
//!
//! ## Architecture
//!
//! ```
//! Watchdog Background Task:
//!   ├── Check all active tokens every 10s
//!   │   ├── heartbeat_at + 3x interval < now → timeout
//!   │   │   ├── 1st timeout: warn + 30s grace
//!   │   │   ├── 2nd timeout: 60s grace
//!   │   │   └── 3rd timeout: reclaim locks + broadcast error
//!   │   └── heartbeat normal → continue
//!   │
//!   └── Monitor ACP connections
//!       └── Connection drop → immediately reclaim all tokens + locks
//! ```

use crate::error::{ErgataiError, ErgataiResult};
use crate::file_access::lock_manager::FileLockManager;
use crate::nats::event_bus::EventBus;
use crate::nats::events::FileErrorPayload;
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration as TokioDuration};
use tracing::{debug, error, info, warn};

/// Watchdog configuration
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Check interval in seconds (default: 10s)
    pub check_interval_secs: u64,
    /// Heartbeat timeout multiplier (default: 3x)
    pub timeout_multiplier: u32,
    /// Grace period for 1st timeout in seconds (default: 30s)
    pub grace_period_1_secs: u64,
    /// Grace period for 2nd timeout in seconds (default: 60s)
    pub grace_period_2_secs: u64,
    /// Enable task-aware heartbeat (default: true)
    pub task_aware: bool,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 10,
            timeout_multiplier: 3,
            grace_period_1_secs: 30,
            grace_period_2_secs: 60,
            task_aware: true,
        }
    }
}

/// Token timeout state (for progressive timeout)
#[derive(Debug, Clone, PartialEq)]
enum TimeoutState {
    /// No timeout yet
    Normal,
    /// 1st timeout detected, in grace period
    GracePeriod1 { since: chrono::DateTime<Utc> },
    /// 2nd timeout detected, in grace period
    GracePeriod2 { since: chrono::DateTime<Utc> },
}

/// Watchdog for monitoring token expiration and ACP disconnects
pub struct Watchdog {
    /// Lock manager for querying/updating tokens
    lock_manager: Arc<FileLockManager>,
    /// NATS event bus for broadcasting file.error events
    event_bus: Option<EventBus>,
    /// Configuration
    config: WatchdogConfig,
    /// Timeout state for each token (token_id → state)
    timeout_states: Arc<Mutex<HashMap<String, TimeoutState>>>,
    /// Task-aware busy status (session_id → busy_until)
    busy_status: Arc<Mutex<HashMap<String, chrono::DateTime<Utc>>>>,
    /// Shutdown signal
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Watchdog {
    /// Create a new watchdog instance
    pub fn new(lock_manager: Arc<FileLockManager>, config: WatchdogConfig) -> Self {
        Self {
            lock_manager,
            event_bus: None,
            config,
            timeout_states: Arc::new(Mutex::new(HashMap::new())),
            busy_status: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx: None,
        }
    }

    /// Set the NATS event bus for broadcasting file.error events
    pub fn with_event_bus(mut self, event_bus: EventBus) -> Self {
        self.event_bus = Some(event_bus);
        self
    }

    /// Start the watchdog background task
    pub fn start(&mut self) -> ErgataiResult<()> {
        if self.shutdown_tx.is_some() {
            return Err(ErgataiError::InvalidArgument(
                "Watchdog already started".to_string(),
            ));
        }

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(tx);

        let lock_manager = Arc::clone(&self.lock_manager);
        let event_bus = self.event_bus.clone();
        let config = self.config.clone();
        let timeout_states = Arc::clone(&self.timeout_states);
        let busy_status = Arc::clone(&self.busy_status);

        tokio::spawn(async move {
            let mut check_interval = interval(TokioDuration::from_secs(config.check_interval_secs));

            info!("Watchdog started with check interval {}s", config.check_interval_secs);

            loop {
                tokio::select! {
                    _ = check_interval.tick() => {
                        if let Err(e) = Self::check_tokens(
                            &lock_manager,
                            &event_bus,
                            &config,
                            &timeout_states,
                            &busy_status,
                        ).await {
                            error!("Watchdog check failed: {}", e);
                        }
                    }
                    _ = &mut rx => {
                        info!("Watchdog received shutdown signal");
                        break;
                    }
                }
            }

            info!("Watchdog stopped");
        });

        Ok(())
    }

    /// Stop the watchdog background task
    pub fn stop(&mut self) -> ErgataiResult<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            info!("Watchdog shutdown signal sent");
        }
        Ok(())
    }

    /// Mark a session as busy (task-aware heartbeat)
    ///
    /// When an agent is executing a long-running task, it can call this to extend
    /// the heartbeat timeout. The watchdog will not reclaim locks until `busy_until`.
    pub async fn mark_busy(
        &self,
        session_id: &str,
        busy_duration_secs: u64,
    ) -> ErgataiResult<()> {
        if !self.config.task_aware {
            debug!("Task-aware heartbeat disabled, ignoring mark_busy");
            return Ok(());
        }

        let busy_until = Utc::now() + Duration::seconds(busy_duration_secs as i64);
        let mut busy_status = self.busy_status.lock().await;
        busy_status.insert(session_id.to_string(), busy_until);

        info!(
            session_id = session_id,
            busy_until = %busy_until,
            "Session marked as busy"
        );

        Ok(())
    }

    /// Clear busy status for a session
    pub async fn clear_busy(&self, session_id: &str) -> ErgataiResult<()> {
        let mut busy_status = self.busy_status.lock().await;
        if busy_status.remove(session_id).is_some() {
            info!(session_id = session_id, "Session busy status cleared");
        }
        Ok(())
    }

    /// Check all active tokens for heartbeat timeout
    async fn check_tokens(
        lock_manager: &Arc<FileLockManager>,
        event_bus: &Option<EventBus>,
        config: &WatchdogConfig,
        timeout_states: &Arc<Mutex<HashMap<String, TimeoutState>>>,
        busy_status: &Arc<Mutex<HashMap<String, chrono::DateTime<Utc>>>>,
    ) -> ErgataiResult<()> {
        let now = Utc::now();

        // Get all active tokens
        let active_tokens = lock_manager.get_active_tokens()?;

        debug!("Checking {} active tokens", active_tokens.len());

        // Phase 1: Compute state transitions while holding locks briefly.
        // Collect actions to apply after releasing locks to avoid triple-nested
        // locking (timeout_states → busy_status → conn inside reclaim).
        struct StateAction {
            token_id: String,
            session_id: String,
            kind: StateActionKind,
        }
        enum StateActionKind {
            SetState(TimeoutState),
            RemoveState,
            Reclaim,
        }

        let actions: Vec<StateAction> = {
            let mut states = timeout_states.lock().await;
            let busy = busy_status.lock().await;

            let mut actions = Vec::new();

            for token in &active_tokens {
                let token_id = token.id.as_str().to_string();
                let session_id = token.session_id.clone();

                // Check if session is busy
                if let Some(busy_until) = busy.get(&session_id) {
                    if now < *busy_until {
                        debug!(
                            token_id = %token_id,
                            session_id = %session_id,
                            busy_until = %busy_until,
                            "Session is busy, skipping timeout check"
                        );
                        actions.push(StateAction {
                            token_id,
                            session_id,
                            kind: StateActionKind::RemoveState,
                        });
                        continue;
                    }
                }

                // Calculate timeout threshold
                let heartbeat_interval = token.heartbeat_interval_secs as i64;
                let timeout_threshold = Duration::seconds(
                    heartbeat_interval * config.timeout_multiplier as i64,
                );
                let time_since_heartbeat = now - token.heartbeat_at;

                // Check if timeout
                if time_since_heartbeat > timeout_threshold {
                    let state = states.entry(token_id.clone()).or_insert(TimeoutState::Normal);

                    match state {
                        TimeoutState::Normal => {
                            warn!(
                                token_id = %token_id,
                                session_id = %session_id,
                                time_since_heartbeat = time_since_heartbeat.num_seconds(),
                                threshold = timeout_threshold.num_seconds(),
                                "Heartbeat timeout detected, entering grace period 1"
                            );
                            actions.push(StateAction {
                                token_id,
                                session_id,
                                kind: StateActionKind::SetState(TimeoutState::GracePeriod1 { since: now }),
                            });
                        }
                        TimeoutState::GracePeriod1 { since } => {
                            let grace_duration = Duration::seconds(config.grace_period_1_secs as i64);
                            if now - *since > grace_duration {
                                warn!(
                                    token_id = %token_id,
                                    session_id = %session_id,
                                    "Grace period 1 expired, entering grace period 2"
                                );
                                actions.push(StateAction {
                                    token_id,
                                    session_id,
                                    kind: StateActionKind::SetState(TimeoutState::GracePeriod2 { since: now }),
                                });
                            }
                        }
                        TimeoutState::GracePeriod2 { since } => {
                            let grace_duration = Duration::seconds(config.grace_period_2_secs as i64);
                            if now - *since > grace_duration {
                                error!(
                                    token_id = %token_id,
                                    session_id = %session_id,
                                    "Grace period 2 expired, reclaiming locks"
                                );
                                actions.push(StateAction {
                                    token_id,
                                    session_id,
                                    kind: StateActionKind::Reclaim,
                                });
                            }
                        }
                    }
                } else {
                    // Heartbeat normal, reset state
                    actions.push(StateAction {
                        token_id,
                        session_id,
                        kind: StateActionKind::RemoveState,
                    });
                }
            }

            actions
        }; // states and busy locks released here

        // Phase 2: Apply state updates (brief lock, no nested calls)
        {
            let mut states = timeout_states.lock().await;
            for action in &actions {
                match &action.kind {
                    StateActionKind::SetState(new_state) => {
                        states.insert(action.token_id.clone(), new_state.clone());
                    }
                    StateActionKind::RemoveState => {
                        states.remove(&action.token_id);
                    }
                    StateActionKind::Reclaim => {
                        states.remove(&action.token_id);
                    }
                }
            }
        } // states lock released here

        // Phase 3: Reclaim locks (no timeout_states or busy_status held)
        for action in &actions {
            if let StateActionKind::Reclaim = action.kind {
                if let Err(e) = Self::reclaim_locks_for_token(
                    lock_manager,
                    event_bus,
                    &action.token_id,
                    &action.session_id,
                )
                .await
                {
                    error!(
                        token_id = %action.token_id,
                        error = %e,
                        "Failed to reclaim locks"
                    );
                }
            }
        }

        Ok(())
    }

    /// Reclaim all locks for a token and broadcast error events
    async fn reclaim_locks_for_token(
        lock_manager: &Arc<FileLockManager>,
        event_bus: &Option<EventBus>,
        token_id: &str,
        session_id: &str,
    ) -> ErgataiResult<()> {
        // Get all locks held by this token
        let locks = lock_manager.get_locks_by_token(token_id)?;

        info!(
            token_id = token_id,
            session_id = session_id,
            lock_count = locks.len(),
            "Reclaiming locks for expired token"
        );

        // Release each lock and broadcast error event
        for lock in &locks {
            let file_path = &lock.file_path;

            // Release the lock
            if let Err(e) = lock_manager.release_lock(token_id, file_path) {
                error!(
                    token_id = token_id,
                    file_path = file_path,
                    error = %e,
                    "Failed to release lock"
                );
            }

            // Broadcast file.error event (Phase 5: v1.1 fix #2)
            // This unblocks READ_LATEST waiters
            if let Some(bus) = event_bus {
                let payload = FileErrorPayload {
                    file_path: file_path.clone(),
                    agent_id: lock.agent_id.clone(),
                    reason: format!("Token {} expired or ACP disconnected", token_id),
                    timestamp: Utc::now().timestamp() as u64,
                };

                if let Err(e) = bus.publish_file_error(&payload).await {
                    error!(
                        token_id = token_id,
                        file_path = file_path,
                        error = %e,
                        "Failed to broadcast file.error event"
                    );
                } else {
                    info!(
                        token_id = token_id,
                        file_path = file_path,
                        "Broadcast file.error event"
                    );
                }
            } else {
                warn!(
                    token_id = token_id,
                    file_path = file_path,
                    session_id = session_id,
                    "Event bus not configured, cannot broadcast file.error event"
                );
            }
        }

        // Mark token as expired
        lock_manager.expire_token(token_id)?;

        info!(
            token_id = token_id,
            "Token expired and locks reclaimed"
        );

        Ok(())
    }

    /// Handle ACP disconnect (immediately reclaim all tokens for session)
    pub async fn handle_acp_disconnect(&self, session_id: &str) -> ErgataiResult<()> {
        info!(session_id = session_id, "ACP disconnect detected, reclaiming all tokens");

        // Get all active locks for this session (by session_id, not token_id)
        let locks = self.lock_manager.get_locks_by_session(session_id)?;
        let lock_count = locks.len();

        // Get all system tokens for this session (to expire them)
        let tokens = self.lock_manager.get_tokens_by_session(session_id)?;
        let token_count = tokens.len();

        // Collect token IDs to expire (release timeout_states lock before reclaiming)
        let token_ids: Vec<String> = {
            let mut states = self.timeout_states.lock().await;
            let ids: Vec<String> = tokens.iter().map(|t| t.id.as_str().to_string()).collect();
            for id in &ids {
                states.remove(id);
            }
            ids
        }; // timeout_states lock released here

        // Clear busy status (separate lock acquisition, no nesting with reclaim)
        {
            let mut busy = self.busy_status.lock().await;
            busy.remove(session_id);
        }

        // Release each lock using its own token_id (FileToken ID, not SystemToken ID)
        for lock in &locks {
            if let Err(e) = self.lock_manager.release_lock(lock.token_id.as_str(), &lock.file_path) {
                error!(
                    session_id = session_id,
                    file_path = %lock.file_path,
                    token_id = %lock.token_id,
                    error = %e,
                    "Failed to release lock on ACP disconnect"
                );
            }

            // Broadcast file.error event
            if let Some(bus) = &self.event_bus {
                let payload = FileErrorPayload {
                    file_path: lock.file_path.clone(),
                    agent_id: lock.agent_id.clone(),
                    reason: format!("ACP session {} disconnected", session_id),
                    timestamp: Utc::now().timestamp() as u64,
                };
                if let Err(e) = bus.publish_file_error(&payload).await {
                    error!(
                        session_id = session_id,
                        file_path = %lock.file_path,
                        error = %e,
                        "Failed to broadcast file.error event"
                    );
                }
            }
        }

        // Expire all system tokens for this session
        for token_id in &token_ids {
            if let Err(e) = self.lock_manager.expire_token(token_id) {
                error!(
                    token_id = token_id,
                    error = %e,
                    "Failed to expire token on ACP disconnect"
                );
            }
        }

        info!(
            session_id = session_id,
            token_count = token_count,
            lock_count = lock_count,
            "All tokens and locks reclaimed on ACP disconnect"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_lock_manager() -> (TempDir, Arc<FileLockManager>) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_locks.db");
        let project_root = temp_dir.path().to_path_buf();

        let lock_manager = Arc::new(
            FileLockManager::new(&db_path, project_root).unwrap(),
        );

        (temp_dir, lock_manager)
    }

    #[tokio::test]
    async fn test_watchdog_start_stop() {
        let (_temp_dir, lock_manager) = create_test_lock_manager();
        let config = WatchdogConfig::default();
        let mut watchdog = Watchdog::new(lock_manager, config);

        // Start watchdog
        watchdog.start().unwrap();

        // Stop watchdog
        watchdog.stop().unwrap();
    }

    #[tokio::test]
    async fn test_mark_busy() {
        let (_temp_dir, lock_manager) = create_test_lock_manager();
        let config = WatchdogConfig::default();
        let watchdog = Watchdog::new(lock_manager, config);

        // Mark session as busy
        watchdog.mark_busy("session-1", 300).await.unwrap();

        // Clear busy status
        watchdog.clear_busy("session-1").await.unwrap();
    }

    #[tokio::test]
    async fn test_task_aware_disabled() {
        let (_temp_dir, lock_manager) = create_test_lock_manager();
        let config = WatchdogConfig {
            task_aware: false,
            ..Default::default()
        };
        let watchdog = Watchdog::new(lock_manager, config);

        // Should not error, just ignore
        watchdog.mark_busy("session-1", 300).await.unwrap();
    }
}
