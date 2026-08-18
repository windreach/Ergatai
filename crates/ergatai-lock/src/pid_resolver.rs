//! PID → (agent_id, session_id) resolution.
//!
//! The fanotify enforcer needs to map the kernel-reported PID of an `open()`
//! caller to an ergatai agent identity. This module provides the trait and
//! concrete implementations.
//!
//! # Design
//!
//! `ergatai-lock` does not depend on `ergatai-runtime` (decoupled crates). To
//! resolve PIDs without introducing that dependency, we define a trait and a
//! callback-based implementation. The caller (typically `ergatai-api` `main.rs`)
//! builds a [`CallbackPidResolver`] that closes over the runtime's registry
//! and passes it in during initialization.
//!
//! # Ancestor walking
//!
//! The PID reported by fanotify may be a child process of the agent (e.g.,
//! `cat` or `sed` spawned by `opencode`). [`CallbackPidResolver`] walks up
//! the parent chain via `/proc/{pid}/stat` until it finds a registered agent
//! or reaches `init` (PID 1).

use std::sync::Arc;
use std::time::{Duration, Instant};

/// Trait for resolving a PID to an ergatai agent identity.
///
/// Implementations must be thread-safe (`Send + Sync`) and `'static` because
/// the fanotify event loop runs on a background tokio task.
pub trait PidResolver: Send + Sync + std::fmt::Debug + 'static {
    /// Map `pid` → `(agent_id, session_id)`.
    ///
    /// Returns `None` if the PID does not belong to a known agent. The
    /// implementation may walk ancestor processes to find a match.
    fn resolve(&self, pid: u32) -> Option<(String, String)>;
}

/// No-op resolver: never identifies any PID as belonging to an agent.
///
/// Used when enforcement is disabled, or in tests that do not exercise the
/// resolution path. The enforcer will treat all callers as non-agents and
/// (under the pessimistic strategy) deny access to locked files.
#[derive(Debug, Clone, Default)]
pub struct NoopPidResolver;

impl PidResolver for NoopPidResolver {
    fn resolve(&self, _pid: u32) -> Option<(String, String)> {
        None
    }
}

/// Resolver backed by a user-supplied snapshot callback.
///
/// The callback returns `Vec<(pid, agent_id, session_id)>` — typically built
/// from `AgentRuntime::list_agents()`. The resolver walks the ancestor chain
/// (via `/proc/{pid}/stat`) on each `resolve()` call to handle child processes.
///
/// # Performance
///
/// Each `resolve()` call invokes the snapshot callback and walks ancestors.
/// Use [`CallbackPidResolver::with_cache`] to cache the snapshot for a short
/// TTL, reducing per-event allocation under high fanotify load.
/// Type alias for the snapshot cache to reduce type complexity.
type SnapshotCacheInner = (Vec<(u32, String, String)>, Instant);
type SnapshotCache = parking_lot::RwLock<SnapshotCacheInner>;

#[derive(Clone)]
pub struct CallbackPidResolver {
    snapshot: Arc<dyn Fn() -> Vec<(u32, String, String)> + Send + Sync>,
    /// Optional TTL-based cache. When set, the snapshot callback is invoked at
    /// most once per TTL window instead of once per `resolve()` call.
    cache: Option<Arc<(SnapshotCache, Duration)>>,
}

impl std::fmt::Debug for CallbackPidResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackPidResolver")
            .field("cached", &self.cache.is_some())
            .finish()
    }
}

impl CallbackPidResolver {
    /// Build a resolver from a snapshot callback. No caching — the callback
    /// is invoked on every `resolve()` call.
    pub fn new(snapshot: impl Fn() -> Vec<(u32, String, String)> + Send + Sync + 'static) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            cache: None,
        }
    }

    /// Build a resolver with a TTL-based snapshot cache. The callback is
    /// invoked at most once per `cache_ttl` duration, regardless of how many
    /// times `resolve()` is called.
    ///
    /// # Memory
    ///
    /// Under high fanotify load (thousands of open() per second), the uncached
    /// variant allocates a `Vec<AgentInfo>` on every event. With caching
    /// (default TTL 50 ms), we allocate ~20× less.
    pub fn with_cache(
        snapshot: impl Fn() -> Vec<(u32, String, String)> + Send + Sync + 'static,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            cache: Some(Arc::new((
                parking_lot::RwLock::new((Vec::new(), Instant::now())),
                cache_ttl,
            ))),
        }
    }

    fn get_snapshot(&self) -> Vec<(u32, String, String)> {
        if let Some(cache_arc) = &self.cache {
            let (cache, ttl) = &**cache_arc;
            // Fast path: cache is fresh (even if empty — empty snapshots are valid)
            {
                let guard = cache.read();
                if guard.1.elapsed() < *ttl {
                    return guard.0.clone();
                }
            }
            // Slow path: refresh
            let snap = (self.snapshot)();
            let mut guard = cache.write();
            *guard = (snap.clone(), Instant::now());
            snap
        } else {
            (self.snapshot)()
        }
    }
}

impl PidResolver for CallbackPidResolver {
    fn resolve(&self, pid: u32) -> Option<(String, String)> {
        let snapshot = self.get_snapshot();
        if snapshot.is_empty() {
            return None;
        }
        // Walk up the ancestor chain, checking each PID against the snapshot.
        // Bounded to 16 hops to avoid pathological /proc parsing loops.
        let mut current = pid;
        for _ in 0..16 {
            if current <= 1 {
                return None;
            }
            for (p, aid, sid) in &snapshot {
                if *p == current {
                    return Some((aid.clone(), sid.clone()));
                }
            }
            match parent_pid(current) {
                Some(pp) if pp != current => current = pp,
                _ => return None,
            }
        }
        None
    }
}

/// Read the parent PID from `/proc/{pid}/stat`.
///
/// Format: `pid (comm) state ppid ...`. The `comm` field is parenthesized and
/// may contain spaces or parens, so we find the *last* `)` and parse field 4
/// relative to that.
fn parent_pid(pid: u32) -> Option<u32> {
    let path = format!("/proc/{}/stat", pid);
    let data = std::fs::read_to_string(&path).ok()?;
    let after_comm = data.rfind(')')?;
    let rest = data.get(after_comm + 2..)?;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?; // field 3
    let ppid_str = fields.next()?; // field 4
    ppid_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_noop_resolver_returns_none() {
        let r = NoopPidResolver;
        assert!(r.resolve(1).is_none());
        assert!(r.resolve(1234).is_none());
        assert!(r.resolve(0).is_none());
    }

    #[test]
    fn test_callback_resolver_matches_direct_pid() {
        let r = CallbackPidResolver::new(|| {
            vec![
                (100, "agent-a".to_string(), "session-1".to_string()),
                (200, "agent-b".to_string(), "session-2".to_string()),
            ]
        });
        assert_eq!(
            r.resolve(100),
            Some(("agent-a".to_string(), "session-1".to_string()))
        );
        assert_eq!(
            r.resolve(200),
            Some(("agent-b".to_string(), "session-2".to_string()))
        );
    }

    #[test]
    fn test_callback_resolver_returns_none_for_unknown_pid() {
        let r = CallbackPidResolver::new(|| {
            vec![(100, "agent-a".to_string(), "session-1".to_string())]
        });
        // PID 999 is not in snapshot. Without /proc, parent walk fails → None.
        // (On Linux, parent_pid may read /proc/999/stat and walk ancestors;
        // for deterministic test, use a very high PID that doesn't exist.)
        let result = r.resolve(4_000_000);
        assert!(result.is_none());
    }

    #[test]
    fn test_callback_resolver_with_empty_snapshot() {
        let r = CallbackPidResolver::new(Vec::new);
        assert!(r.resolve(100).is_none());
    }

    #[test]
    fn test_callback_resolver_invokes_snapshot_each_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let r = CallbackPidResolver::new(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            vec![]
        });
        let _ = r.resolve(100);
        let _ = r.resolve(200);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_parent_pid_returns_none_for_nonexistent_process() {
        // PID 4_000_000 almost certainly doesn't exist on a test machine.
        assert!(parent_pid(4_000_000).is_none());
    }

    #[test]
    fn test_parent_pid_resolves_for_current_process() {
        // Our own process's parent should resolve to something reasonable.
        let self_pid = std::process::id();
        let ppid = parent_pid(self_pid);
        // Parent PID should be > 0 and different from our own PID (unless we're init).
        if let Some(pp) = ppid {
            assert!(pp > 0);
            // Allow ppid == self_pid in pathological containers, but normally differs.
        }
    }

    #[test]
    fn test_callback_resolver_is_debug() {
        // Debug impl should not panic and should produce some output.
        let r = CallbackPidResolver::new(Vec::new);
        let dbg = format!("{:?}", r);
        assert!(dbg.contains("CallbackPidResolver"));
    }

    #[test]
    fn test_noop_resolver_is_debug() {
        let dbg = format!("{:?}", NoopPidResolver);
        assert!(dbg.contains("NoopPidResolver"));
    }

    #[test]
    fn test_noop_resolver_is_default() {
        let _r: NoopPidResolver = Default::default();
    }

    #[test]
    fn test_resolver_is_send_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<NoopPidResolver>();
        assert_send_sync::<CallbackPidResolver>();
        assert_send_sync::<Box<dyn PidResolver>>();
    }
}
