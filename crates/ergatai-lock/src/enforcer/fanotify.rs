//! Linux fanotify backend for kernel-level file access enforcement.
//!
//! Uses the `fanotify` API to intercept `open()` syscalls at the VFS layer,
//! receive `FAN_OPEN_PERM` permission events, and respond with `FAN_ALLOW`
//! or `FAN_DENY`. This is the only mandatory enforcement backend; on non-Linux
//! platforms the enforcer falls back to advisory-only mode.
//!
//! # Fail-open invariant
//!
//! Every permission event MUST receive a kernel response — even if path
//! resolution fails, the decision engine panics, or the NATS publish errors.
//! The [`KernelResponseGuard`] RAII helper enforces this: on drop, if no
//! explicit response has been written, it writes `FAN_ALLOW` and closes the
//! per-event fd. We never leave a userspace process blocked in the kernel.

use std::collections::VecDeque;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{error, warn};

use super::backend::{EnforcementResult, EnforcerBackend, FileAccessEvent, PlatformHandle};

/// Read `/proc/self/fd/{fd}` via readlink. Returns `None` on any error.
pub(crate) fn readlink_proc_fd(fd: i32) -> Option<PathBuf> {
    let link = format!("/proc/self/fd/{}", fd);
    std::fs::read_link(&link).ok()
}

/// RAII guard ensuring the kernel always receives a fanotify response.
///
/// # Invariant
///
/// The kernel is blocked waiting for a response to this permission event.
/// If we drop this guard without having written a response (e.g., due to a
/// panic in the decision path), we MUST write `FAN_ALLOW` to unblock the
/// process. We also close `meta_fd` on drop — fanotify requires this.
struct KernelResponseGuard {
    group_fd: RawFd,
    meta_fd: RawFd,
    responded: bool,
}

impl KernelResponseGuard {
    fn new(group_fd: RawFd, meta_fd: RawFd) -> Self {
        Self {
            group_fd,
            meta_fd,
            responded: false,
        }
    }

    /// Write a fanotify response. Idempotent — subsequent calls are no-ops.
    fn respond_now(&mut self, allow: bool) {
        if self.responded {
            return;
        }
        let response = libc::fanotify_response {
            fd: self.meta_fd,
            response: if allow {
                libc::FAN_ALLOW
            } else {
                libc::FAN_DENY
            },
        };
        // SAFETY: `group_fd` is a valid open fanotify group fd (owned by `FanotifyBackend`).
        // `response` is a stack-local `fanotify_response` struct; we pass a pointer to it
        // with the correct size. The kernel reads exactly `size_of::<fanotify_response>()`
        // bytes. The pointer is valid for the duration of the syscall.
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
        // SAFETY: `meta_fd` is a valid open per-event fd provided by the kernel as part
        // of the fanotify permission event. Closing it after responding is required by
        // the fanotify API to release the per-event resource. We only reach here after
        // responding (or fail-open), so the kernel has already processed this event.
        unsafe {
            libc::close(self.meta_fd);
        }
    }
}

/// Internal buffer state for parsing batched fanotify events.
///
/// A single `read()` on the fanotify fd may return multiple events packed
/// sequentially with no alignment guarantee. We parse them into a queue of
/// [`FileAccessEvent`] and drain the queue across successive `next_event()`
/// calls, only reading from the fd again when the queue is empty.
struct ReadBuffer {
    /// Raw byte buffer. Capacity is fixed at 4096 (one page).
    buf: Vec<u8>,
    /// Current parse offset within `buf`.
    offset: usize,
    /// Number of valid bytes in `buf`.
    len: usize,
    /// Parsed events waiting to be yielded.
    pending: VecDeque<FileAccessEvent>,
}

impl ReadBuffer {
    fn new() -> Self {
        Self {
            buf: vec![0u8; 4096],
            offset: 0,
            len: 0,
            pending: VecDeque::new(),
        }
    }

    /// Reset the buffer for a fresh read.
    fn reset_for_read(&mut self) {
        self.offset = 0;
        self.len = 0;
    }
}

/// Linux fanotify backend.
///
/// Owns the fanotify group fd (shared with the facade via `Arc<AtomicI32>`
/// so that `Enforcer::stop()` can force-close it to unblock a wedged event
/// loop) and the `AsyncFd` wrapper for tokio readiness notifications.
pub struct FanotifyBackend {
    /// fanotify group fd. `-1` if closed. Shared with `Enforcer` so that
    /// `stop()` can force-close it.
    fd: Arc<AtomicI32>,
    /// Tokio async wrapper for readiness notifications.
    async_fd: tokio::io::unix::AsyncFd<RawFd>,
    /// Project root. Not used for path stripping (that's the facade's job)
    /// but kept for diagnostics and potential future use.
    _project_root: PathBuf,
    /// Buffered event state. Mutex is tokio's async Mutex because it is
    /// held across `.await` points (when waiting for `readable()`).
    state: Mutex<ReadBuffer>,
}

impl FanotifyBackend {
    /// Initialize fanotify and create a new backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - fanotify_init fails (typically missing `CAP_SYS_ADMIN`)
    /// - fanotify_mark fails
    /// - the project root cannot be canonicalized
    /// - `AsyncFd::new` fails
    /// - the tokio runtime is single-threaded (`block_in_place` would panic)
    pub fn new(project_root: &std::path::Path) -> Result<Self, String> {
        use std::os::unix::io::RawFd;

        // block_in_place requires the multi-threaded runtime. If we're on a
        // current-thread runtime, the first fanotify event would panic — so
        // fail early rather than crashing at runtime.
        if tokio::runtime::Handle::current().runtime_flavor()
            == tokio::runtime::RuntimeFlavor::CurrentThread
        {
            return Err("fanotify enforcer requires multi-thread tokio runtime; \
                 current runtime is single-threaded"
                .to_string());
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
        // SAFETY: `fanotify_init` is a standard libc wrapper around the fanotify_init
        // syscall. We pass valid flag combinations (`FAN_CLASS_CONTENT | FAN_CLOEXEC |
        // FAN_NONBLOCK`) and valid event file flags (`O_NONBLOCK | O_CLOEXEC`). The
        // function returns a new fd on success or -1 on error; we check for both.
        // No memory is accessed through raw pointers here.
        let raw_fd = unsafe { libc::fanotify_init(init_flags, event_f_flags) };
        if raw_fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(format!(
                "fanotify_init failed (need CAP_SYS_ADMIN?): {}",
                err
            ));
        }
        let raw_fd = raw_fd as RawFd;

        // Mark the project root mount for permission events.
        let c_path = match std::ffi::CString::new(project_root.to_string_lossy().as_ref()) {
            Ok(c) => c,
            Err(e) => {
                // SAFETY: `raw_fd` was just returned by `fanotify_init` and is a valid
                // open fd. We close it here because the CString conversion failed and we
                // cannot proceed with initialization. No other code path will close it.
                unsafe {
                    libc::close(raw_fd);
                }
                return Err(format!("invalid path: {}", e));
            }
        };
        // SAFETY: `libc::syscall` is used because `libc::fanotify_mark` is not available
        // on all supported libc versions. Arguments: `raw_fd` is a valid open fanotify
        // group fd; `FAN_MARK_ADD | FAN_MARK_MOUNT` and `FAN_OPEN_PERM` are valid flag
        // combinations; `AT_FDCWD` is a standard sentinel; `c_path.as_ptr()` points to a
        // valid NUL-terminated CString that lives until after the syscall returns.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_fanotify_mark,
                raw_fd,
                libc::FAN_MARK_ADD | libc::FAN_MARK_MOUNT,
                libc::FAN_OPEN_PERM,
                libc::AT_FDCWD,
                c_path.as_ptr(),
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            // SAFETY: `raw_fd` is a valid open fanotify group fd (returned by fanotify_init
            // above). fanotify_mark failed, so we must close the fd ourselves — no other
            // owner exists yet (AsyncFd has not been created).
            unsafe {
                libc::close(raw_fd);
            }
            return Err(format!("fanotify_mark failed: {}", err));
        }

        let async_fd = match tokio::io::unix::AsyncFd::new(raw_fd) {
            Ok(a) => a,
            Err(e) => {
                // SAFETY: `raw_fd` is a valid open fanotify group fd. AsyncFd::new
                // failed to take ownership, so we must close the fd ourselves to avoid
                // leaking it. No other code path will close it.
                unsafe {
                    libc::close(raw_fd);
                }
                return Err(format!("AsyncFd::new failed: {}", e));
            }
        };

        Ok(Self {
            fd: Arc::new(AtomicI32::new(raw_fd)),
            async_fd,
            _project_root: project_root.to_path_buf(),
            state: Mutex::new(ReadBuffer::new()),
        })
    }

    /// Access the shared fd handle (so the facade can force-close on stop).
    pub(crate) fn fd_handle(&self) -> Arc<AtomicI32> {
        self.fd.clone()
    }

    /// Parse events from the read buffer into the pending queue.
    ///
    /// Returns `true` if at least one event was enqueued. Events whose path
    /// cannot be resolved are responded to immediately with `FAN_ALLOW`
    /// (fail-open) and not enqueued — the kernel must not be left waiting.
    fn parse_events_from_buffer(fd: Arc<AtomicI32>, state: &mut ReadBuffer) -> bool {
        let meta_size = std::mem::size_of::<libc::fanotify_event_metadata>();
        let mut found_any = false;
        let group_fd = fd.load(Ordering::SeqCst);

        while state.offset + meta_size <= state.len {
            // SAFETY: The buffer contains raw fanotify events packed sequentially by the
            // kernel. fanotify_event_metadata has no alignment guarantee (events are
            // variable-length and packed), so we must use `read_unaligned`. The pointer
            // `state.buf.as_ptr().add(state.offset)` is within the valid buffer region
            // (checked by the loop condition: `state.offset + meta_size <= state.len`).
            // `event_len` is validated immediately after to prevent reading past the
            // buffer boundary before advancing the offset.
            let meta = unsafe {
                std::ptr::read_unaligned(
                    state.buf.as_ptr().add(state.offset) as *const libc::fanotify_event_metadata
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
            if state.offset + event_len > state.len {
                warn!(
                    offset = state.offset,
                    event_len,
                    len = state.len,
                    "fanotify: event extends past buffer, stopping batch"
                );
                break;
            }

            if meta.mask & libc::FAN_OPEN_PERM != 0 {
                match readlink_proc_fd(meta.fd) {
                    Some(abs_path) => {
                        state.pending.push_back(FileAccessEvent {
                            absolute_path: abs_path,
                            pid: meta.pid as u32,
                            platform_handle: PlatformHandle::Fanotify {
                                group_fd,
                                event_fd: meta.fd,
                            },
                        });
                        found_any = true;
                    }
                    None => {
                        // Path resolution failed — fail open immediately. The
                        // kernel is blocked on this event and must be unblocked.
                        warn!(
                            fd = meta.fd,
                            pid = meta.pid,
                            "fanotify: readlink failed, failing open"
                        );
                        Self::respond_and_close_raw(group_fd, meta.fd, true);
                    }
                }
            }

            state.offset += event_len;
        }

        // Reset buffer when fully consumed so the next read starts fresh.
        if state.offset >= state.len {
            state.reset_for_read();
        }

        found_any
    }

    /// Write a fanotify response and close the per-event fd, without
    /// constructing a guard. Used only when an event is unresolvable and
    /// we need to fail-open from within `next_event()`.
    fn respond_and_close_raw(group_fd: RawFd, meta_fd: RawFd, allow: bool) {
        let response = libc::fanotify_response {
            fd: meta_fd,
            response: if allow {
                libc::FAN_ALLOW
            } else {
                libc::FAN_DENY
            },
        };
        // SAFETY: `group_fd` is a valid open fanotify group fd. `response` is a
        // stack-local struct; we pass a pointer to it with the correct size. This is
        // identical to the safety argument in `KernelResponseGuard::respond_now`.
        let rc = unsafe {
            libc::write(
                group_fd,
                &response as *const _ as *const _,
                std::mem::size_of::<libc::fanotify_response>(),
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            warn!(error = %err, "fanotify: failed to write fail-open response");
        }
        // SAFETY: `meta_fd` is a valid open per-event fd provided by the kernel. We
        // must close it after responding to release the per-event resource. This is
        // called only from the fail-open path inside `parse_events_from_buffer` when
        // readlink fails, so the event will not be processed further.
        unsafe {
            libc::close(meta_fd);
        }
    }
}

#[async_trait]
impl EnforcerBackend for FanotifyBackend {
    fn name(&self) -> &'static str {
        "fanotify"
    }

    fn is_mandatory(&self) -> bool {
        true
    }

    async fn next_event(&self) -> Option<FileAccessEvent> {
        loop {
            // 1. Drain any pending events from a previous read.
            {
                let mut state = self.state.lock().await;
                if let Some(ev) = state.pending.pop_front() {
                    return Some(ev);
                }
                // Try to parse more from unconsumed bytes in the buffer.
                if Self::parse_events_from_buffer(self.fd.clone(), &mut state) {
                    if let Some(ev) = state.pending.pop_front() {
                        return Some(ev);
                    }
                }
            }

            // 2. Wait for the fanotify fd to become readable.
            let mut guard = match self.async_fd.readable().await {
                Ok(g) => g,
                Err(e) => {
                    warn!(error = %e, "fanotify AsyncFd::readable failed, stopping");
                    return None;
                }
            };

            // 3. Read events into the buffer.
            let fd: RawFd = self.async_fd.as_raw_fd();
            let mut state = self.state.lock().await;
            // SAFETY: `fd` is a valid open fanotify group fd (owned by `async_fd`,
            // which wraps it via `AsyncFd`). `state.buf` is a `Vec<u8>` with capacity
            // 4096; `as_mut_ptr()` points to the start of the allocated buffer, valid
            // for `buf.len()` bytes. We set `state.len = n` immediately after to
            // track how many bytes were actually written, preventing over-reads in
            // `parse_events_from_buffer`. The `state` Mutex ensures exclusive access.
            let n = unsafe { libc::read(fd, state.buf.as_mut_ptr() as *mut _, state.buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    guard.clear_ready();
                    continue;
                }
                error!(error = %err, "fanotify read failed");
                guard.clear_ready();
                return None;
            }
            if n == 0 {
                guard.clear_ready();
                continue;
            }
            state.len = n as usize;
            state.offset = 0;
            guard.clear_ready();

            // 4. Parse events and yield the first one (if any).
            Self::parse_events_from_buffer(self.fd.clone(), &mut state);
            if let Some(ev) = state.pending.pop_front() {
                return Some(ev);
            }
            // No usable events in this read — loop back to wait.
        }
    }

    async fn respond(
        &self,
        handle: PlatformHandle,
        result: EnforcementResult,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (group_fd, event_fd) = match handle {
            PlatformHandle::Fanotify { group_fd, event_fd } => (group_fd, event_fd),
            _ => {
                return Err("FanotifyBackend received non-Fanotify handle".into());
            }
        };

        // The guard ensures the kernel always gets a response, even if the
        // write panics. On drop without explicit response, it fails open.
        let mut guard = KernelResponseGuard::new(group_fd, event_fd);
        guard.respond_now(matches!(result, EnforcementResult::Allow));
        // guard dropped here — `responded == true`, so drop only closes event_fd.
        Ok(())
    }

    async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Mark the fd as closed. The actual close is handled by AsyncFd on drop;
        // we must NOT close it here to avoid double-close (AsyncFd owns the fd).
        self.fd.store(-1, Ordering::SeqCst);
        Ok(())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_readlink_proc_fd_returns_none_for_invalid_fd() {
        // fd -1 is always invalid.
        assert!(readlink_proc_fd(-1).is_none());
        // fd 999999 is almost certainly not open.
        assert!(readlink_proc_fd(999_999).is_none());
    }

    #[test]
    fn test_read_buffer_initial_state() {
        let buf = ReadBuffer::new();
        assert_eq!(buf.buf.len(), 4096);
        assert_eq!(buf.offset, 0);
        assert_eq!(buf.len, 0);
        assert!(buf.pending.is_empty());
    }

    #[test]
    fn test_read_buffer_reset_for_read() {
        let mut buf = ReadBuffer::new();
        buf.offset = 100;
        buf.len = 200;
        buf.reset_for_read();
        assert_eq!(buf.offset, 0);
        assert_eq!(buf.len, 0);
    }
}
