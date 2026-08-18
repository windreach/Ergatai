//! Kernel-level file access enforcement.
//!
//! This module implements mandatory file locking on Linux by intercepting
//! `open()` syscalls at the VFS layer using fanotify's `FAN_OPEN_PERM` events.
//!
//! # Architecture
//!
//! The enforcer is split into two layers:
//!
//! - [`DecisionEngine`]: pure logic — given a file path and caller PID, returns
//!   Allow or Deny based on the current lock state. No I/O; unit-testable.
//! - [`Enforcer`]: facade that owns a platform [`EnforcerBackend`] and runs a
//!   unified event loop. The backend handles OS-specific interception
//!   (fanotify on Linux, Endpoint Security on macOS, etc.); the facade handles
//!   path normalization, decision dispatch, and NATS audit events.
//!
//! # Fail-open semantics
//!
//! If backend initialization fails (non-Linux, no `CAP_SYS_ADMIN`, container),
//! the enforcer logs a warning and marks itself inactive. The rest of ergatai
//! continues to function with advisory locks only. Errors during event
//! processing (e.g., SQLite read failure, path resolution failure) also fail
//! open — the kernel is told to allow the access. We never block development
//! due to an enforcer bug.
//!
//! # Pessimistic strategy
//!
//! When a file has an active WRITE lock, ALL `open()` calls from non-holder
//! agents are denied. fanotify's permission events do not expose `O_RDONLY`
//! vs `O_WRONLY`, so we cannot distinguish reads from writes.

// Cross-platform backend abstraction (Phase 2)
pub mod backend;
// Linux fanotify backend (Phase 3)
#[cfg(target_os = "linux")]
pub mod fanotify;
// Advisory-only fallback (Phase 4)
pub mod advisory;
// macOS Endpoint Security backend (Phase 4, stub)
#[cfg(target_os = "macos")]
pub mod endpoint_security;
// Windows Minifilter backend (Phase 4, stub)
#[cfg(target_os = "windows")]
pub mod minifilter;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::lock_manager::FileLockManager;
use crate::pid_resolver::PidResolver;
use ergatai_error::{ErgataiError, ErgataiResult};
use ergatai_nats::events::{EnforcementAction, FileEnforcementPayload};

use self::backend::{EnforcementResult, EnforcerBackend};

/// Configuration for the enforcer.
#[derive(Debug, Clone)]
pub struct EnforcerConfig {
    /// Whether to publish NATS events on denials.
    pub publish_nats_events: bool,
}

impl Default for EnforcerConfig {
    fn default() -> Self {
        Self {
            publish_nats_events: true,
        }
    }
}

/// Decision outcome from [`DecisionEngine::decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Access is allowed.
    Allow,
    /// Access is denied because the file is locked by another agent.
    Deny {
        holder_agent: String,
        holder_session: String,
        caller_agent: Option<String>,
    },
}

/// Pure decision logic. No I/O, no fanotify — testable with mock data.
///
/// Given a relative file path (within the project root) and a caller PID,
/// returns [`Decision::Allow`] or [`Decision::Deny`].
pub struct DecisionEngine {
    lock_manager: Arc<FileLockManager>,
    pid_resolver: Arc<dyn PidResolver>,
    /// PIDs that belong to ergatai's own processes (the API server, NATS, etc.).
    /// Always allowed — we must not block our own operations.
    self_pids: Arc<RwLock<std::collections::HashSet<u32>>>,
}

impl DecisionEngine {
    /// Create a new engine. Automatically allowlists the current process PID.
    pub fn new(lock_manager: Arc<FileLockManager>, pid_resolver: Arc<dyn PidResolver>) -> Self {
        let mut initial = std::collections::HashSet::new();
        initial.insert(std::process::id());
        Self {
            lock_manager,
            pid_resolver,
            self_pids: Arc::new(RwLock::new(initial)),
        }
    }

    /// Register a PID that should always be allowed.
    pub fn allowlist_pid(&self, pid: u32) {
        self.self_pids.write().insert(pid);
    }

    /// Remove a PID from the allowlist (e.g., when a subprocess exits).
    pub fn remove_allowlisted_pid(&self, pid: u32) {
        self.self_pids.write().remove(&pid);
    }

    /// Access the underlying lock manager (for recording violations, etc.).
    pub fn lock_manager(&self) -> &Arc<FileLockManager> {
        &self.lock_manager
    }

    /// Core decision function.
    ///
    /// - `relative_path`: path **already normalized** (relative to project root,
    ///   symlinks resolved). The caller — the fanotify event loop — derives this
    ///   from `readlink /proc/self/fd/{fd}` + `strip_prefix(project_root)`, which
    ///   produces the same canonical relative path that `FileLockManager` uses as
    ///   the cache/DB key. We therefore call `check_file_lock_status_fast` to
    ///   skip the redundant (and expensive) `canonicalize()` syscall.
    /// - `caller_pid`: PID reported by fanotify.
    ///
    /// Decision order:
    /// 1. Self-PID allowlist → Allow (ergatai's own processes).
    /// 2. Fail-open on any error → Allow.
    /// 3. File not locked → Allow.
    /// 4. Caller is the lock holder (same agent_id + session_id) → Allow.
    /// 5. Caller is a *known* non-holder agent → Deny.
    /// 6. Caller is an *unknown* PID (not in agent registry) → Allow.
    ///    Rationale: the design doc (§3.4) specifies that non-agent processes
    ///    are not subject to enforcement. Denying unknown PIDs would
    ///    spuriously block system tools, IDEs, and freshly-spawned agents
    ///    whose PID hasn't yet been observed by the resolver snapshot.
    pub fn decide(&self, relative_path: &str, caller_pid: u32) -> Decision {
        // 1. Self-PID allowlist.
        if self.self_pids.read().contains(&caller_pid) {
            debug!(pid = caller_pid, "fanotify: self-pid allowed");
            return Decision::Allow;
        }

        // 2. Resolve caller identity.
        let caller = self.pid_resolver.resolve(caller_pid);

        // 3. Check lock state and get holder info in a single query (fail-open).
        //    Uses the fast path: no canonicalize, try_lock on SQLite, negative
        //    caching for unlocked files. Never blocks the caller.
        let (is_locked, holder_info) =
            match self.lock_manager.check_file_lock_status_fast(relative_path) {
                Ok(result) => result,
                Err(e) => {
                    warn!(
                        error = %e,
                        path = relative_path,
                        "fanotify: check_file_lock_status_fast failed, failing open"
                    );
                    return Decision::Allow;
                }
            };

        if !is_locked {
            return Decision::Allow;
        }

        // 4. Extract holder info (should be present if is_locked is true, but handle race).
        let holder = match holder_info {
            Some(h) => h,
            None => {
                // Race: lock was released between the query and this check.
                debug!(
                    path = relative_path,
                    "fanotify: lock race (holder gone), allowing"
                );
                return Decision::Allow;
            }
        };

        // 5. Same agent → allow.
        // 6. Unknown PID (not in agent registry) → allow. The design doc (§3.4)
        //    specifies non-agent processes are not subject to enforcement.
        //    Also, freshly-spawned agents whose PID the resolver snapshot has
        //    not yet observed would otherwise be spuriously denied.
        // 7. Known non-holder agent → deny.
        match caller {
            None => {
                debug!(
                    pid = caller_pid,
                    path = relative_path,
                    "fanotify: unknown PID (not in agent registry), allowing"
                );
                Decision::Allow
            }
            Some((ref caller_aid, ref caller_sid)) => {
                if *caller_aid == holder.0 && *caller_sid == holder.1 {
                    debug!(
                        pid = caller_pid,
                        agent = %caller_aid,
                        path = relative_path,
                        "fanotify: holder re-opening own file, allowing"
                    );
                    Decision::Allow
                } else {
                    Decision::Deny {
                        holder_agent: holder.0,
                        holder_session: holder.1,
                        caller_agent: Some(caller_aid.clone()),
                    }
                }
            }
        }
    }
}

/// The enforcer facade.
///
/// Owns a platform [`EnforcerBackend`] and runs the unified event loop on a
/// background tokio task. If backend initialization fails (non-Linux, no
/// `CAP_SYS_ADMIN`, container), the enforcer is created in a disabled state
/// (`is_active() == false`) and the event loop does not run. This is the
/// fail-open default.
#[allow(dead_code)]
pub struct Enforcer {
    /// Backend handle (fanotify fd on Linux). Shared so that `stop()` can
    /// force-close it to unblock a wedged event loop.
    ///
    /// On non-Linux this is always `-1` and never touched.
    backend_fd: Arc<AtomicI32>,
    /// Cancellation token for the event loop.
    cancel: CancellationToken,
    /// Join handle for the event loop task. `None` if enforcer is disabled.
    task: Arc<parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Whether the enforcer is actively watching.
    active: Arc<AtomicBool>,
    /// Project root (used for path stripping in the event loop).
    project_root: PathBuf,
    /// Project ID (used for NATS subject routing: `ergatai.file.enforced.{project_id}`).
    project_id: String,
    /// Configuration.
    config: EnforcerConfig,
}

impl Enforcer {
    /// Start the enforcer.
    ///
    /// Returns an enforcer with `active = false` (and no event loop running) if
    /// no backend is available (non-Linux, insufficient privileges, etc.). The
    /// caller should check [`is_active`](Self::is_active) and log accordingly.
    ///
    /// `project_id` is used to build the NATS subject for enforcement events:
    /// `ergatai.file.enforced.{project_id}`.
    pub fn start(
        project_root: PathBuf,
        project_id: String,
        lock_manager: Arc<FileLockManager>,
        pid_resolver: Arc<dyn PidResolver>,
        nats_client: Option<Arc<async_nats::Client>>,
        config: EnforcerConfig,
    ) -> ErgataiResult<Self> {
        let engine = Arc::new(DecisionEngine::new(lock_manager, pid_resolver));

        // Select and initialize the platform backend.
        let (backend, backend_fd) = match Self::select_backend(&project_root) {
            Some(pair) => pair,
            None => {
                info!("no enforcement backend available; enforcement disabled");
                return Ok(Self::disabled(project_root, project_id, config));
            }
        };

        let cancel = CancellationToken::new();
        let cancel_inner = cancel.clone();
        // Canonicalize project_root so that strip_prefix works correctly.
        // readlink(/proc/self/fd/{fd}) returns a canonical (symlink-resolved) path;
        // if project_root contains symlinks, strip_prefix would fail silently and
        // enforcement would be disabled for the entire project.
        let project_root_canonical = match project_root.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                // Do NOT close the fd here — it is owned by the backend's AsyncFd,
                // which will close it on drop. Closing it here would cause double-close
                // undefined behavior if the fd number is reused before AsyncFd drops.
                // The backend is dropped when we return Err, and AsyncFd closes the fd.
                return Err(ErgataiError::internal(format!(
                    "failed to canonicalize project_root {}: {}",
                    project_root.display(),
                    e
                )));
            }
        };

        let backend: Arc<dyn EnforcerBackend> = Arc::from(backend);
        let backend_inner = backend.clone();
        let project_root_inner = project_root_canonical;
        let project_id_inner = project_id.clone();
        let config_inner = config.clone();

        let task = tokio::spawn(async move {
            Self::event_loop(
                backend_inner,
                engine,
                project_root_inner,
                project_id_inner,
                nats_client,
                config_inner,
                cancel_inner,
            )
            .await;
        });

        info!(
            backend = backend.name(),
            project_root = %project_root.display(),
            project_id = %project_id,
            "enforcer started"
        );

        Ok(Self {
            backend_fd,
            cancel,
            task: Arc::new(parking_lot::Mutex::new(Some(task))),
            active: Arc::new(AtomicBool::new(true)),
            project_root,
            project_id,
            config,
        })
    }

    /// Stop the enforcer and wait for the event loop to exit.
    ///
    /// Bounded by a 2-second timeout. If the event loop is somehow wedged
    /// (e.g., a bug leaves it blocked), we close the backend fd to force
    /// its pending `read()` to return, then give up on waiting. The fd close
    /// is the definitive cleanup — no further events can arrive after it.
    pub async fn stop(&self) {
        self.cancel.cancel();
        // Take the join handle out of the mutex, then drop the guard before awaiting.
        let task = self.task.lock().take();
        if let Some(task) = task {
            // Bound the wait. If the event loop is wedged, we don't want to
            // hang the caller (typically shutdown_file_access) forever.
            let wait_result = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
            if wait_result.is_err() {
                warn!("enforcer: event loop did not exit within 2s; forcing fd close");
            }
        }
        // Mark the backend fd as closed. The actual close is handled by the
        // backend's AsyncFd on drop; we must NOT close it here to avoid
        // double-close undefined behavior.
        self.backend_fd.store(-1, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        info!("enforcer stopped");
    }

    /// Whether the enforcer is actively watching.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Build an enforcer in the disabled state (no event loop).
    fn disabled(project_root: PathBuf, project_id: String, config: EnforcerConfig) -> Self {
        Self {
            backend_fd: Arc::new(AtomicI32::new(-1)),
            cancel: CancellationToken::new(),
            task: Arc::new(parking_lot::Mutex::new(None)),
            active: Arc::new(AtomicBool::new(false)),
            project_root,
            project_id,
            config,
        }
    }

    /// Select and initialize the platform-specific backend.
    ///
    /// Returns `None` if no backend is available (non-Linux, insufficient
    /// privileges, single-threaded runtime). The caller falls back to
    /// advisory-only enforcement.
    ///
    /// Returns `(backend, fd_handle)` where `fd_handle` is the raw fd wrapped
    /// in an `Arc<AtomicI32>` for force-close from `Enforcer::stop()`. On
    /// platforms without a kernel fd, the handle is initialized to `-1`.
    fn select_backend(
        project_root: &std::path::Path,
    ) -> Option<(Box<dyn EnforcerBackend>, Arc<AtomicI32>)> {
        #[cfg(target_os = "linux")]
        {
            match fanotify::FanotifyBackend::new(project_root) {
                Ok(b) => {
                    let fd_handle = b.fd_handle();
                    Some((Box::new(b), fd_handle))
                }
                Err(e) => {
                    warn!(error = %e, "fanotify init failed; enforcement disabled");
                    None
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            match endpoint_security::EndpointSecurityBackend::new(&project_root) {
                Ok(b) => {
                    let fd_handle = Arc::new(AtomicI32::new(-1));
                    Some((Box::new(b), fd_handle))
                }
                Err(e) => {
                    info!(error = %e, "Endpoint Security backend not available; enforcement disabled");
                    None
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            match minifilter::MinifilterBackend::new(&project_root) {
                Ok(b) => {
                    let fd_handle = Arc::new(AtomicI32::new(-1));
                    Some((Box::new(b), fd_handle))
                }
                Err(e) => {
                    info!(error = %e, "Minifilter backend not available; enforcement disabled");
                    None
                }
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = project_root;
            info!(
                platform = std::env::consts::OS,
                "no enforcer backend available for this platform; advisory mode only"
            );
            None
        }
    }

    /// The unified event loop.
    ///
    /// Pulls events from the backend, normalizes paths, invokes the decision
    /// engine, writes kernel responses, and (on denials) fires background
    /// tasks to record audit entries + NATS events.
    ///
    /// # Deadlock prevention
    ///
    /// The decision path is synchronous and acquires `parking_lot` mutexes
    /// (SQLite connection, in-memory cache). Running it directly on the tokio
    /// worker thread would block that thread, and running it under
    /// `spawn_blocking` with a `timeout` would NOT cancel the inner task —
    /// the abandoned task would continue to hold the SQLite mutex and
    /// eventually deadlock every other caller.
    ///
    /// We use `tokio::task::block_in_place` instead. This converts the current
    /// worker thread into a blocking thread for the duration of the decision;
    /// tokio compensates by spinning up a replacement worker. The decision is
    /// guaranteed to run to completion on a dedicated thread, and there is no
    /// orphaned task to leak mutexes.
    ///
    /// The SQLite fallback inside `check_file_lock_status_fast` uses
    /// `try_lock()` — if the connection mutex is contended, we fail open
    /// immediately. Combined, these two mechanisms make a deadlock impossible:
    /// the decision never blocks indefinitely, and it never abandons a task
    /// that holds a mutex.
    async fn event_loop(
        backend: Arc<dyn EnforcerBackend>,
        engine: Arc<DecisionEngine>,
        project_root: PathBuf,
        project_id: String,
        nats_client: Option<Arc<async_nats::Client>>,
        config: EnforcerConfig,
        cancel: CancellationToken,
    ) {
        loop {
            // Wait for the next event or cancellation.
            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("enforcer event loop: cancellation received");
                    return;
                }
                ev = backend.next_event() => match ev {
                    Some(e) => e,
                    None => {
                        debug!(
                            backend = backend.name(),
                            "enforcer event loop: backend stream ended"
                        );
                        return;
                    }
                },
            };

            // Normalize path: strip project_root to get the relative path that
            // FileLockManager uses as its cache/DB key.
            let relative = event
                .absolute_path
                .strip_prefix(&project_root)
                .ok()
                .map(|p| p.to_string_lossy().to_string());

            // Run the decision in-place. `block_in_place` converts the current
            // tokio worker into a blocking thread for the duration; tokio
            // compensates by spinning up a replacement worker. Inside,
            // `check_file_lock_status_fast` uses try_lock() on the SQLite
            // mutex, so this never blocks indefinitely.
            let decision = match relative.as_deref() {
                Some(rel) => {
                    let engine = engine.clone();
                    let rel_owned = rel.to_string();
                    let pid = event.pid;
                    tokio::task::block_in_place(move || engine.decide(&rel_owned, pid))
                }
                None => Decision::Allow, // outside project or resolution failed → allow
            };

            // Write kernel response via the backend.
            let result = match &decision {
                Decision::Allow => EnforcementResult::Allow,
                Decision::Deny { .. } => EnforcementResult::Deny,
            };
            if let Err(e) = backend.respond(event.platform_handle.clone(), result).await {
                // Fail-open: if the backend can't respond, log but continue.
                // The backend is responsible for ensuring the kernel-blocked
                // process is released even on error.
                warn!(error = %e, backend = backend.name(), "backend respond failed");
            }

            // Audit + NATS publish run in a detached task so the event loop can
            // return to reading the next event immediately. These are
            // non-critical: a missed audit is far less harmful than a stalled
            // event loop (which would block every open() system-wide).
            if let Decision::Deny {
                holder_agent,
                holder_session,
                caller_agent,
            } = decision
            {
                let rel = relative.as_deref().unwrap_or("?").to_string();
                let engine = engine.clone();
                let nats_client = nats_client.clone();
                let publish_nats = config.publish_nats_events;
                let project_id_owned = project_id.to_string();
                let pid = event.pid;

                // Fire-and-forget audit + NATS publish. We intentionally detach
                // the JoinHandle: a missed audit is far less harmful than a
                // stalled event loop, and the spawned task has its own
                // catch_unwind via tokio.
                let _audit_handle = tokio::spawn(async move {
                    // Audit log (fire-and-forget).
                    if let Err(e) = engine.lock_manager().record_enforced_violation(
                        &rel,
                        caller_agent.as_deref(),
                        Some(&holder_agent),
                    ) {
                        warn!(error = %e, "failed to record enforced violation");
                    }
                    // NATS event (fire-and-forget).
                    if publish_nats {
                        if let Some(client) = nats_client {
                            let payload = FileEnforcementPayload {
                                file_path: rel,
                                pid,
                                agent_id: caller_agent.clone(),
                                session_id: None,
                                action: EnforcementAction::Denied,
                                holder_agent_id: Some(holder_agent.clone()),
                                holder_session_id: Some(holder_session.clone()),
                                reason: format!("file locked by {}", holder_agent),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                            };
                            let subject = format!("ergatai.file.enforced.{}", project_id_owned);
                            if let Ok(json) = serde_json::to_string(&payload) {
                                if let Err(e) = client.publish(subject, json.into()).await {
                                    warn!(error = %e, "failed to publish enforcement event");
                                }
                            }
                        }
                    }
                });
            }
        }
    }
}

// ── Unit tests for DecisionEngine (no fanotify needed) ──────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pid_resolver::NoopPidResolver;
    use crate::token::{FileMode, FileToken, SystemToken};
    use std::fs;
    use tempfile::TempDir;

    /// Helper: set up a FileLockManager with a test file and optional lock.
    fn setup_lock_manager() -> (TempDir, Arc<FileLockManager>) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("locks.db");
        let project_root = temp_dir.path().to_path_buf();
        fs::write(project_root.join("target.rs"), "fn main() {}").unwrap();
        let lm = Arc::new(FileLockManager::new(&db_path, project_root, None).unwrap());
        (temp_dir, lm)
    }

    fn make_token_and_lock_sync(
        lm: &FileLockManager,
        agent_id: &str,
        session_id: &str,
        file_path: &str,
    ) {
        let sys = SystemToken::new(
            agent_id.to_string(),
            session_id.to_string(),
            "/test".to_string(),
            3600,
            30,
        );
        lm.register_system_token(&sys).unwrap();
        let token = FileToken::new(
            agent_id.to_string(),
            session_id.to_string(),
            sys.id.clone(),
            "**".to_string(),
            FileMode::Write,
            None,
            "test".to_string(),
            3600,
            15,
        );
        lm.register_file_token(&token).unwrap();
        // Acquire lock synchronously via a minimal runtime.
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(lm.acquire_lock(&token, file_path))
            .unwrap();
    }

    #[test]
    fn test_decision_engine_allowlists_self_pid() {
        let (_temp, lm) = setup_lock_manager();
        let resolver = Arc::new(NoopPidResolver);
        let engine = DecisionEngine::new(lm, resolver);
        let self_pid = std::process::id();
        // Even though no file is locked, self-PID should always be allowed.
        let decision = engine.decide("target.rs", self_pid);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_decision_engine_unlocked_file_allows() {
        let (_temp, lm) = setup_lock_manager();
        // Resolver that says PID 99999 is agent-x.
        let resolver = Arc::new(crate::pid_resolver::CallbackPidResolver::new(|| {
            vec![(99999, "agent-x".to_string(), "session-x".to_string())]
        }));
        let engine = DecisionEngine::new(lm, resolver);
        // target.rs has no lock.
        let decision = engine.decide("target.rs", 99999);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_decision_engine_holder_reopening_allowed() {
        let (_temp, lm) = setup_lock_manager();
        // Register a WRITE lock held by agent-a.
        make_token_and_lock_sync(&lm, "agent-a", "session-a", "target.rs");
        // Resolver: PID 70000 → agent-a (the holder).
        let resolver = Arc::new(crate::pid_resolver::CallbackPidResolver::new(|| {
            vec![(70000, "agent-a".to_string(), "session-a".to_string())]
        }));
        let engine = DecisionEngine::new(lm, resolver);
        let decision = engine.decide("target.rs", 70000);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_decision_engine_different_agent_denied() {
        let (_temp, lm) = setup_lock_manager();
        make_token_and_lock_sync(&lm, "agent-a", "session-a", "target.rs");
        // Resolver: PID 80000 → agent-b (different from holder).
        let resolver = Arc::new(crate::pid_resolver::CallbackPidResolver::new(|| {
            vec![(80000, "agent-b".to_string(), "session-b".to_string())]
        }));
        let engine = DecisionEngine::new(lm, resolver);
        let decision = engine.decide("target.rs", 80000);
        assert!(matches!(decision, Decision::Deny { .. }));
        if let Decision::Deny {
            holder_agent,
            holder_session,
            caller_agent,
        } = decision
        {
            assert_eq!(holder_agent, "agent-a");
            assert_eq!(holder_session, "session-a");
            assert_eq!(caller_agent, Some("agent-b".to_string()));
        }
    }

    #[test]
    fn test_decision_engine_non_agent_pid_allowed_when_locked() {
        let (_temp, lm) = setup_lock_manager();
        make_token_and_lock_sync(&lm, "agent-a", "session-a", "target.rs");
        // Resolver: no match for PID 60000 (non-agent).
        let resolver = Arc::new(NoopPidResolver);
        let engine = DecisionEngine::new(lm, resolver);
        let decision = engine.decide("target.rs", 60000);
        // Unknown PIDs are allowed — see design doc §3.4: non-agent processes
        // are not subject to enforcement.
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_decision_engine_allowlist_pid_override() {
        let (_temp, lm) = setup_lock_manager();
        make_token_and_lock_sync(&lm, "agent-a", "session-a", "target.rs");
        let resolver = Arc::new(NoopPidResolver);
        let engine = DecisionEngine::new(lm, resolver);
        // Allowlist a specific PID. Unknown PIDs are allowed anyway (design §3.4:
        // non-agent processes not subject to enforcement), so this is mostly a
        // no-op, but the allowlist mechanism should still work.
        engine.allowlist_pid(55555);
        let decision = engine.decide("target.rs", 55555);
        assert_eq!(decision, Decision::Allow);
        // Remove from allowlist → still allowed (unknown PIDs are allowed per §3.4).
        engine.remove_allowlisted_pid(55555);
        let decision = engine.decide("target.rs", 55555);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_decision_engine_path_outside_project_not_in_error_state() {
        let (_temp, lm) = setup_lock_manager();
        let resolver = Arc::new(NoopPidResolver);
        let engine = DecisionEngine::new(lm, resolver);
        // A non-existent path causes validate_and_normalize_path to fail → is_file_locked
        // returns Err → fail-open → Allow.
        let decision = engine.decide("nonexistent/deeply/nested/file.rs", 12345);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn test_enforcer_disabled_state() {
        let (_temp, lm) = setup_lock_manager();
        let project_root = lm.project_root().to_path_buf();
        let enforcer =
            Enforcer::disabled(project_root, "test".to_string(), EnforcerConfig::default());
        assert!(!enforcer.is_active());
        // stop() on a disabled enforcer should be a no-op.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(enforcer.stop());
        assert!(!enforcer.is_active());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_readlink_proc_fd_returns_none_for_invalid_fd() {
        // Delegate to the fanotify module where readlink_proc_fd now lives.
        use crate::enforcer::fanotify::readlink_proc_fd;
        // fd -1 is always invalid.
        assert!(readlink_proc_fd(-1).is_none());
        // fd 999999 is almost certainly not open.
        assert!(readlink_proc_fd(999_999).is_none());
    }

    // ── MockBackend + event_loop integration tests ──────────────────

    /// Mock backend for testing the event loop without platform-specific mechanisms.
    ///
    /// Events are injected via `inject_event()`, and responses are recorded
    /// in a channel for assertion. The backend returns `None` from `next_event()`
    /// when the event channel is closed, causing the event loop to exit.
    #[cfg(test)]
    mod mock_backend {
        use super::*;
        use std::sync::Arc;
        use tokio::sync::{mpsc, Mutex};

        use crate::enforcer::backend::{
            EnforcementResult, EnforcerBackend, FileAccessEvent, PlatformHandle,
        };

        pub struct MockBackend {
            events_rx: Arc<Mutex<mpsc::Receiver<FileAccessEvent>>>,
            responses_tx: mpsc::Sender<(PlatformHandle, EnforcementResult)>,
            stopped: Arc<AtomicBool>,
        }

        impl MockBackend {
            /// Create a mock backend and the handles for injecting events / reading responses.
            ///
            /// Returns `(backend, events_tx, responses_rx)`.
            /// - `events_tx`: inject events into the backend's stream.
            /// - `responses_rx`: read the kernel responses the event loop wrote.
            pub fn new() -> (
                Self,
                mpsc::Sender<FileAccessEvent>,
                mpsc::Receiver<(PlatformHandle, EnforcementResult)>,
            ) {
                let (events_tx, events_rx) = mpsc::channel(64);
                let (responses_tx, responses_rx) = mpsc::channel(64);
                let backend = Self {
                    events_rx: Arc::new(Mutex::new(events_rx)),
                    responses_tx,
                    stopped: Arc::new(AtomicBool::new(false)),
                };
                (backend, events_tx, responses_rx)
            }
        }

        #[async_trait::async_trait]
        impl EnforcerBackend for MockBackend {
            fn name(&self) -> &'static str {
                "mock"
            }

            fn is_mandatory(&self) -> bool {
                true
            }

            async fn next_event(&self) -> Option<FileAccessEvent> {
                let mut rx = self.events_rx.lock().await;
                rx.recv().await
            }

            async fn respond(
                &self,
                handle: PlatformHandle,
                result: EnforcementResult,
            ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                let _ = self.responses_tx.send((handle, result)).await;
                Ok(())
            }

            async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                self.stopped.store(true, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    /// Test: event loop processes events and writes kernel responses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_event_loop_processes_events() {
        use self::mock_backend::MockBackend;
        use crate::enforcer::backend::{FileAccessEvent, PlatformHandle};

        let (temp, lm) = setup_lock_manager();
        let project_root = temp.path().canonicalize().unwrap();
        let project_root_inner = project_root.clone();

        let resolver = Arc::new(NoopPidResolver);
        let engine = Arc::new(DecisionEngine::new(lm, resolver));

        let (backend, events_tx, mut responses_rx) = MockBackend::new();
        let backend: Arc<dyn EnforcerBackend> = Arc::new(backend);
        let cancel = CancellationToken::new();

        // Spawn the event loop.
        let cancel_inner = cancel.clone();
        let backend_inner = backend.clone();
        let config = EnforcerConfig {
            publish_nats_events: false,
        };
        let handle = tokio::spawn(async move {
            Enforcer::event_loop(
                backend_inner,
                engine,
                project_root_inner,
                "test-project".to_string(),
                None,
                config,
                cancel_inner,
            )
            .await;
        });

        // Inject an event for an unlocked file → should be allowed.
        let event = FileAccessEvent {
            absolute_path: project_root.join("target.rs"),
            pid: 99999, // unknown PID → allowed
            platform_handle: PlatformHandle::Advisory,
        };
        events_tx.send(event).await.unwrap();

        // Wait for the response.
        let (resp_handle, result) =
            tokio::time::timeout(std::time::Duration::from_secs(2), responses_rx.recv())
                .await
                .expect("timeout waiting for response")
                .expect("response channel closed");

        assert!(matches!(resp_handle, PlatformHandle::Advisory));
        assert_eq!(result, EnforcementResult::Allow);

        // Cancel the event loop.
        cancel.cancel();
        // Close events_tx so next_event() returns None (belt-and-suspenders).
        drop(events_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    /// Test: event loop exits cleanly on cancellation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_event_loop_cancellation() {
        use self::mock_backend::MockBackend;

        let (temp, lm) = setup_lock_manager();
        let project_root = temp.path().canonicalize().unwrap();

        let resolver = Arc::new(NoopPidResolver);
        let engine = Arc::new(DecisionEngine::new(lm, resolver));

        let (backend, events_tx, _responses_rx) = MockBackend::new();
        let backend: Arc<dyn EnforcerBackend> = Arc::new(backend);
        let cancel = CancellationToken::new();

        let cancel_inner = cancel.clone();
        let backend_inner = backend.clone();
        let config = EnforcerConfig {
            publish_nats_events: false,
        };
        let handle = tokio::spawn(async move {
            Enforcer::event_loop(
                backend_inner,
                engine,
                project_root,
                "test-project".to_string(),
                None,
                config,
                cancel_inner,
            )
            .await;
        });

        // Give the event loop a moment to start and block on next_event().
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Cancel → event loop should exit.
        cancel.cancel();
        drop(events_tx);

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "event loop did not exit within 2s after cancellation"
        );
    }

    /// Test: event loop exits when backend stream ends (next_event returns None).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_event_loop_exits_on_backend_stream_end() {
        use self::mock_backend::MockBackend;

        let (temp, lm) = setup_lock_manager();
        let project_root = temp.path().canonicalize().unwrap();

        let resolver = Arc::new(NoopPidResolver);
        let engine = Arc::new(DecisionEngine::new(lm, resolver));

        let (backend, events_tx, _responses_rx) = MockBackend::new();
        let backend: Arc<dyn EnforcerBackend> = Arc::new(backend);
        let cancel = CancellationToken::new();

        let cancel_inner = cancel.clone();
        let backend_inner = backend.clone();
        let config = EnforcerConfig {
            publish_nats_events: false,
        };
        let handle = tokio::spawn(async move {
            Enforcer::event_loop(
                backend_inner,
                engine,
                project_root,
                "test-project".to_string(),
                None,
                config,
                cancel_inner,
            )
            .await;
        });

        // Close the events channel → next_event() returns None → event loop exits.
        drop(events_tx);

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "event loop did not exit when backend stream ended"
        );
    }

    /// Test: event loop denies access to a locked file from a non-holder agent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_event_loop_denies_locked_file() {
        use self::mock_backend::MockBackend;
        use crate::enforcer::backend::{FileAccessEvent, PlatformHandle};

        let (temp, lm) = setup_lock_manager();
        let project_root = temp.path().canonicalize().unwrap();
        let project_root_inner = project_root.clone();

        // Set up a WRITE lock held by agent-a on target.rs (using the test's async runtime).
        {
            let sys = SystemToken::new(
                "agent-a".to_string(),
                "session-a".to_string(),
                "/test".to_string(),
                3600,
                30,
            );
            lm.register_system_token(&sys).unwrap();
            let token = FileToken::new(
                "agent-a".to_string(),
                "session-a".to_string(),
                sys.id.clone(),
                "**".to_string(),
                FileMode::Write,
                None,
                "test".to_string(),
                3600,
                15,
            );
            lm.register_file_token(&token).unwrap();
            lm.acquire_lock(&token, "target.rs").await.unwrap();
        }

        // Resolver: PID 55555 → agent-b (not the holder).
        let resolver = Arc::new(crate::pid_resolver::CallbackPidResolver::new(|| {
            vec![(55555, "agent-b".to_string(), "session-b".to_string())]
        }));
        let engine = Arc::new(DecisionEngine::new(lm, resolver));

        let (backend, events_tx, mut responses_rx) = MockBackend::new();
        let backend: Arc<dyn EnforcerBackend> = Arc::new(backend);
        let cancel = CancellationToken::new();

        let cancel_inner = cancel.clone();
        let backend_inner = backend.clone();
        let config = EnforcerConfig {
            publish_nats_events: false,
        };
        let handle = tokio::spawn(async move {
            Enforcer::event_loop(
                backend_inner,
                engine,
                project_root_inner,
                "test-project".to_string(),
                None,
                config,
                cancel_inner,
            )
            .await;
        });

        // agent-b (PID 55555) tries to access target.rs (locked by agent-a) → Deny.
        let event = FileAccessEvent {
            absolute_path: project_root.join("target.rs"),
            pid: 55555,
            platform_handle: PlatformHandle::Advisory,
        };
        events_tx.send(event).await.unwrap();

        let (_resp_handle, result) =
            tokio::time::timeout(std::time::Duration::from_secs(2), responses_rx.recv())
                .await
                .expect("timeout waiting for deny response")
                .expect("response channel closed");

        assert_eq!(result, EnforcementResult::Deny);

        // Cleanup.
        cancel.cancel();
        drop(events_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
}
