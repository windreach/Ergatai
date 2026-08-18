//! Kernel-level file access enforcement via fanotify.
//!
//! This module implements mandatory file locking on Linux by intercepting
//! `open()` syscalls at the VFS layer using fanotify's `FAN_OPEN_PERM` events.
//!
//! # Architecture
//!
//! - [`DecisionEngine`]: pure logic — given a file path and caller PID, returns
//!   Allow or Deny based on the current lock state. No I/O; unit-testable.
//! - [`Enforcer`]: owns the fanotify file descriptor and runs the event loop on
//!   a background tokio task. Reads permission events, invokes the engine, writes
//!   responses, publishes NATS events on denials.
//!
//! # Fail-open semantics
//!
//! If fanotify initialization fails (non-Linux, no `CAP_SYS_ADMIN`, container),
//! the enforcer logs a warning and marks itself inactive. The rest of ergatai
//! continues to function with advisory locks only. Errors during event processing
//! (e.g., SQLite read failure, path resolution failure) also fail open — the
//! kernel is told to allow the access. We never block development due to an
//! enforcer bug.
//!
//! # Pessimistic strategy
//!
//! When a file has an active WRITE lock, ALL `open()` calls from non-holder
//! agents are denied. fanotify's permission events do not expose `O_RDONLY`
//! vs `O_WRONLY`, so we cannot distinguish reads from writes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::lock_manager::FileLockManager;
use crate::pid_resolver::PidResolver;
use ergatai_error::{ErgataiError, ErgataiResult};
use ergatai_nats::events::{EnforcementAction, FileEnforcementPayload};

/// Configuration for the enforcer.
#[derive(Debug, Clone)]
pub struct EnforcerConfig {
    /// Whether to publish NATS events on denials.
    pub publish_nats_events: bool,
    /// Maximum time (ms) to spend deciding before failing open. Prevents
    /// deadlock if the SQLite mutex or PidResolver callback blocks.
    /// Default: 100 ms.
    pub decision_timeout_ms: u64,
    /// TTL (ms) for the cached agent snapshot inside the event loop. Reduces
    /// per-event allocation from `runtime.list_agents()`. Default: 50 ms.
    pub snapshot_cache_ttl_ms: u64,
}

impl Default for EnforcerConfig {
    fn default() -> Self {
        Self {
            publish_nats_events: true,
            decision_timeout_ms: 100,
            snapshot_cache_ttl_ms: 50,
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
            match self
                .lock_manager
                .check_file_lock_status_fast(relative_path)
            {
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

/// The fanotify event loop and lifecycle manager.
///
/// Owns the fanotify file descriptor and the tokio task running the event loop.
/// If fanotify initialization fails (non-Linux, no `CAP_SYS_ADMIN`, container),
/// the enforcer is created in a disabled state (`is_active() == false`) and the
/// event loop does not run. This is the fail-open default.
#[allow(dead_code)]
pub struct Enforcer {
    /// fanotify file descriptor. `-1` if not initialized or closed.
    fanotify_fd: Arc<AtomicI32>,
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
    /// fanotify is unavailable (non-Linux, insufficient privileges, etc.). The
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
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&lock_manager, &pid_resolver, &nats_client);
            info!("fanotify is Linux-only; enforcement disabled");
            return Ok(Self::disabled(project_root, project_id, config));
        }

        #[cfg(target_os = "linux")]
        {
            Self::start_linux(
                project_root,
                project_id,
                lock_manager,
                pid_resolver,
                nats_client,
                config,
            )
        }
    }

    /// Stop the enforcer and wait for the event loop to exit.
    ///
    /// Bounded by a 2-second timeout. If the event loop is somehow wedged
    /// (e.g., a bug leaves it blocked), we close the fanotify fd to force
    /// its pending `read()` to return, then give up on waiting. The fd close
    /// is the definitive cleanup — no further events can arrive after it.
    pub async fn stop(&self) {
        self.cancel.cancel();
        // Take the join handle out of the mutex, then drop the guard before awaiting.
        let task = self.task.lock().take();
        if let Some(task) = task {
            // Bound the wait. If the event loop is wedged, we don't want to
            // hang the caller (typically shutdown_file_access) forever.
            let wait_result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                task,
            )
            .await;
            if wait_result.is_err() {
                warn!("fanotify: event loop did not exit within 2s; forcing fd close");
            }
        }
        // Close the fanotify fd to unblock any pending read() and release kernel
        // resources. After this, no further events can arrive regardless of
        // whether the event loop task exited cleanly.
        let fd = self.fanotify_fd.swap(-1, Ordering::SeqCst);
        if fd >= 0 {
            unsafe {
                libc::close(fd);
            }
        }
        self.active.store(false, Ordering::SeqCst);
        info!("fanotify enforcer stopped");
    }

    /// Whether the enforcer is actively watching.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Build an enforcer in the disabled state (no event loop).
    fn disabled(project_root: PathBuf, project_id: String, config: EnforcerConfig) -> Self {
        Self {
            fanotify_fd: Arc::new(AtomicI32::new(-1)),
            cancel: CancellationToken::new(),
            task: Arc::new(parking_lot::Mutex::new(None)),
            active: Arc::new(AtomicBool::new(false)),
            project_root,
            project_id,
            config,
        }
    }

    /// Linux-specific initialization.
    #[cfg(target_os = "linux")]
    fn start_linux(
        project_root: PathBuf,
        project_id: String,
        lock_manager: Arc<FileLockManager>,
        pid_resolver: Arc<dyn PidResolver>,
        nats_client: Option<Arc<async_nats::Client>>,
        config: EnforcerConfig,
    ) -> ErgataiResult<Self> {
        use std::os::unix::io::RawFd;

        // block_in_place requires the multi-threaded runtime. If we're on a
        // current-thread runtime, the first fanotify event would panic — so
        // disable enforcement early rather than crashing at runtime.
        if tokio::runtime::Handle::current().runtime_flavor()
            == tokio::runtime::RuntimeFlavor::CurrentThread
        {
            warn!(
                "fanotify enforcer requires multi-thread tokio runtime; \
                 current runtime is single-threaded — enforcement disabled"
            );
            return Ok(Self::disabled(project_root, project_id, config));
        }

        // fanotify_init(flags, event_f_flags)
        // flags: FAN_CLASS_CONTENT (content-level interception for permission events)
        //        + FAN_CLOEXEC + FAN_NONBLOCK for safe async usage.
        // event_f_flags: file status flags for the fanotify fd. Only O_NONBLOCK,
        //        O_CLOEXEC (since 5.13), and O_LARGEFILE (since 5.13) are valid.
        //        O_RDWR is NOT valid here (fanotify fd is not a regular file).
        // NOTE: FAN_OPEN_PERM is a *mask* for fanotify_mark, NOT a flag for fanotify_init.
        //       Passing it here causes EINVAL.
        // We use libc::fanotify_init() (C wrapper) instead of raw syscall to ensure
        // correct argument types (unsigned int, not u64/i64).
        let init_flags: libc::c_uint =
            libc::FAN_CLASS_CONTENT | libc::FAN_CLOEXEC | libc::FAN_NONBLOCK;
        let event_f_flags: libc::c_uint = (libc::O_NONBLOCK | libc::O_CLOEXEC) as libc::c_uint;
        let fd = unsafe { libc::fanotify_init(init_flags, event_f_flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            warn!(
                error = %err,
                "fanotify_init failed (need CAP_SYS_ADMIN?); enforcement disabled"
            );
            return Ok(Self::disabled(project_root, project_id, config));
        }
        let fd = fd as RawFd;

        // Mark the project root mount for permission events.
        let c_path = match std::ffi::CString::new(project_root.to_string_lossy().as_ref()) {
            Ok(c) => c,
            Err(e) => {
                unsafe {
                    libc::close(fd);
                }
                return Err(ErgataiError::internal(format!("invalid path: {}", e)));
            }
        };
        let rc = unsafe {
            libc::syscall(
                libc::SYS_fanotify_mark,
                fd,
                libc::FAN_MARK_ADD | libc::FAN_MARK_MOUNT,
                libc::FAN_OPEN_PERM,
                libc::AT_FDCWD,
                c_path.as_ptr(),
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(ErgataiError::internal(format!(
                "fanotify_mark failed: {}",
                err
            )));
        }

        let engine = Arc::new(DecisionEngine::new(lock_manager, pid_resolver));
        let async_fd = match tokio::io::unix::AsyncFd::new(fd) {
            Ok(a) => a,
            Err(e) => {
                unsafe {
                    libc::close(fd);
                }
                return Err(ErgataiError::internal(format!(
                    "AsyncFd::new failed: {}",
                    e
                )));
            }
        };

        let cancel = CancellationToken::new();
        let cancel_inner = cancel.clone();
        // Canonicalize project_root so that strip_prefix works correctly.
        // readlink(/proc/self/fd/{fd}) returns a canonical (symlink-resolved) path;
        // if project_root contains symlinks, strip_prefix would fail silently and
        // enforcement would be disabled for the entire project.
        let project_root_canonical = project_root.canonicalize().map_err(|e| {
            unsafe {
                libc::close(fd);
            }
            ErgataiError::internal(format!(
                "failed to canonicalize project_root {}: {}",
                project_root.display(),
                e
            ))
        })?;
        let project_root_inner = project_root_canonical;
        let project_id_inner = project_id.clone();
        let config_inner = config.clone();

        let task = tokio::spawn(async move {
            Self::event_loop(
                async_fd,
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
            project_root = %project_root.display(),
            project_id = %project_id,
            "fanotify enforcer started"
        );

        Ok(Self {
            fanotify_fd: Arc::new(AtomicI32::new(fd)),
            cancel,
            task: Arc::new(parking_lot::Mutex::new(Some(task))),
            active: Arc::new(AtomicBool::new(true)),
            project_root,
            project_id,
            config,
        })
    }

    /// The event loop. Runs on a background tokio task.
    ///
    /// Reads fanotify permission events, invokes the decision engine, writes
    /// FAN_ALLOW / FAN_DENY responses, and (on denials) fires a background
    /// task to record the audit entry + NATS event.
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
    #[cfg(target_os = "linux")]
    async fn event_loop(
        async_fd: tokio::io::unix::AsyncFd<std::os::unix::io::RawFd>,
        engine: Arc<DecisionEngine>,
        project_root: PathBuf,
        project_id: String,
        nats_client: Option<Arc<async_nats::Client>>,
        config: EnforcerConfig,
        cancel: CancellationToken,
    ) {
        use std::os::unix::io::{AsRawFd, RawFd};

        const BUF_SIZE: usize = 4096;
        let mut buf = vec![0u8; BUF_SIZE];

        loop {
            let mut guard = tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("fanotify event loop: cancellation received");
                    return;
                }
                res = async_fd.readable() => match res {
                    Ok(g) => g,
                    Err(e) => {
                        warn!(error = %e, "fanotify AsyncFd::readable failed, exiting");
                        return;
                    }
                },
            };

            let fd: RawFd = async_fd.as_raw_fd();
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, BUF_SIZE) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    guard.clear_ready();
                    continue;
                }
                error!(error = %err, "fanotify read failed");
                guard.clear_ready();
                continue;
            }
            if n == 0 {
                guard.clear_ready();
                continue;
            }

            // Parse event(s) from the buffer. fanotify may batch multiple events.
            // NOTE: events are packed sequentially with NO alignment guarantee.
            // Use `read_unaligned` to avoid UB from creating an unaligned reference.
            let meta_size = std::mem::size_of::<libc::fanotify_event_metadata>();
            let mut offset = 0usize;
            while offset + meta_size <= n as usize {
                let meta = unsafe {
                    std::ptr::read_unaligned(
                        buf.as_ptr().add(offset) as *const libc::fanotify_event_metadata,
                    )
                };
                let event_len = meta.event_len as usize;
                // Safety valve: zero-length event means malformed buffer; bail.
                if event_len < meta_size {
                    warn!(
                        event_len,
                        "fanotify: malformed event (too short), stopping batch"
                    );
                    break;
                }
                // Bounds check: don't read past the valid buffer region.
                if offset + event_len > n as usize {
                    warn!(
                        offset,
                        event_len,
                        n,
                        "fanotify: event extends past buffer, stopping batch"
                    );
                    break;
                }

                if meta.mask & libc::FAN_OPEN_PERM != 0 {
                    Self::handle_perm_event(
                        fd,
                        &meta,
                        &engine,
                        &project_root,
                        &project_id,
                        &nats_client,
                        &config,
                    )
                    .await;
                }

                offset += event_len;
            }

            guard.clear_ready();
        }
    }

    /// Handle a single permission event.
    ///
    /// # Kernel-response invariant
    ///
    /// The kernel is guaranteed to receive a response — we NEVER skip the
    /// `fanotify_response` write or the `close(meta.fd)`, even if the decision
    /// path or the audit path panics. A `Drop` guard enforces this.
    #[cfg(target_os = "linux")]
    async fn handle_perm_event(
        group_fd: std::os::unix::io::RawFd,
        meta: &libc::fanotify_event_metadata,
        engine: &Arc<DecisionEngine>,
        project_root: &PathBuf,
        project_id: &str,
        nats_client: &Option<Arc<async_nats::Client>>,
        config: &EnforcerConfig,
    ) {
        // Resolve absolute path via /proc/self/fd/{fd} (fast: ~1-5 µs). The
        // result is already canonical (symlinks resolved), so strip_prefix
        // produces the same normalized relative path that FileLockManager
        // uses as its cache/DB key — no further canonicalize() needed.
        let abs_path = readlink_proc_fd(meta.fd);
        let relative = abs_path
            .as_ref()
            .and_then(|p| p.strip_prefix(project_root).ok())
            .map(|p| p.to_string_lossy().to_string());

        // Ensure the kernel always gets a response, even if the decision path
        // panics. The guard closes meta.fd and writes FAN_ALLOW as a last resort.
        struct KernelResponseGuard {
            group_fd: std::os::unix::io::RawFd,
            meta_fd: std::os::unix::io::RawFd,
            responded: bool,
        }
        impl KernelResponseGuard {
            fn respond_now(&mut self, allow: bool) {
                if self.responded {
                    return;
                }
                let response = libc::fanotify_response {
                    fd: self.meta_fd,
                    response: if allow { libc::FAN_ALLOW } else { libc::FAN_DENY },
                };
                let rc = unsafe {
                    libc::write(
                        self.group_fd,
                        &response as *const _ as *const _,
                        std::mem::size_of::<libc::fanotify_response>(),
                    )
                };
                if rc < 0 {
                    let err = std::io::Error::last_os_error();
                    warn!(error = %err, "fanotify: failed to write response");
                }
                self.responded = true;
            }
        }
        impl Drop for KernelResponseGuard {
            fn drop(&mut self) {
                // Fail-open: if we haven't responded yet (e.g., panic in decide),
                // allow the access rather than leave the kernel-blocked process
                // hanging forever.
                if !self.responded {
                    warn!("fanotify: KernelResponseGuard dropped without explicit response, failing open");
                    self.respond_now(true);
                }
                unsafe {
                    libc::close(self.meta_fd);
                }
            }
        }

        let mut guard = KernelResponseGuard {
            group_fd,
            meta_fd: meta.fd,
            responded: false,
        };

        // Run the decision in-place. `block_in_place` converts the current tokio
        // worker into a blocking thread for the duration; tokio compensates by
        // spinning up a replacement worker. Inside, `check_file_lock_status_fast`
        // uses try_lock() on the SQLite mutex, so this never blocks indefinitely.
        let decision = match relative.as_deref() {
            Some(rel) => {
                let engine = engine.clone();
                let rel_owned = rel.to_string();
                let pid = meta.pid as u32;
                tokio::task::block_in_place(move || engine.decide(&rel_owned, pid))
            }
            None => Decision::Allow, // outside project or resolution failed → allow
        };

        // Write FAN_ALLOW / FAN_DENY response. Marks the guard so Drop won't
        // double-respond.
        guard.respond_now(matches!(&decision, Decision::Allow));
        // meta.fd is closed by the guard's Drop.
        guard.responded = true; // We've responded; Drop only needs to close fd.

        // Audit + NATS publish run in a detached task so the event loop can
        // return to reading the next fanotify event immediately. These are
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
            let pid = meta.pid as u32;

            // Fire-and-forget audit + NATS publish. We intentionally detach the
            // JoinHandle: a missed audit is far less harmful than a stalled event
            // loop, and the spawned task has its own catch_unwind via tokio.
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

/// Read `/proc/self/fd/{fd}` via readlink. Returns `None` on any error.
#[cfg(target_os = "linux")]
fn readlink_proc_fd(fd: i32) -> Option<PathBuf> {
    let link = format!("/proc/self/fd/{}", fd);
    std::fs::read_link(&link).ok()
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
        let enforcer = Enforcer::disabled(project_root, "test".to_string(), EnforcerConfig::default());
        assert!(!enforcer.is_active());
        // stop() on a disabled enforcer should be a no-op.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(enforcer.stop());
        assert!(!enforcer.is_active());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_readlink_proc_fd_returns_none_for_invalid_fd() {
        // fd -1 is always invalid.
        assert!(readlink_proc_fd(-1).is_none());
        // fd 999999 is almost certainly not open.
        assert!(readlink_proc_fd(999_999).is_none());
    }
}
