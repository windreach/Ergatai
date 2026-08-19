# Fanotify-Based File Lock Enforcement Design

> **Status**: Draft
> **Author**: fanotify-design agent
> **Created**: 2026-08-18
> **Related Crates**: `ergatai-lock`, `ergatai-runtime`, `ergatai-nats`

---

## 1. Executive Summary

Ergatai's current file locking is purely advisory: agents request `send_message` / acquire lock via the MCP protocol, but can bypass this with shell commands (`echo x > file`, `sed -i`, `vim`). The existing `FileSystemWatcher` (Phase 6) uses the `notify` crate for **detect-after-the-fact** monitoring — it can only log violations, not prevent them.

This design introduces **Linux fanotify** `FAN_OPEN_PERM` permission events to intercept write `open()` system calls at the kernel level, implementing true **mandatory locking**.

### 1.1 Core Value

| Dimension | Current (advisory) | fanotify (mandatory) |
|-----------|-------------------|---------------------|
| Bypass difficulty | Any shell command can bypass | Kernel-level interception; cannot bypass without root |
| Detection timing | After file is modified (notify event) | Before file is opened (permission event) |
| Violation cost | Post-hoc audit + alert | Operation fails immediately (EACCES/EPERM) |
| Agent experience | Unaware | Receives clear "file locked by agent X" error |

---

## 2. Existing Architecture Analysis

### 2.1 Key Components

```
┌──────────────────────────────────────────────────────────────────┐
│  FileAccessManager (manager.rs)                                  │
│  ─────────────────────────────────────────────────────────────── │
│  OnceLock<RwLock<HashMap<project_id, ProjectFileAccess>>>        │
│  └── ProjectFileAccess {                                         │
│        lock_manager:     Arc<FileLockManager>,   ← SQLite lock DB│
│        snapshot_manager: Arc<SnapshotManager>,   ← Git COW      │
│        watchdog:         Arc<RwLock<Watchdog>>,  ← heartbeat/exp │
│        // ✨ New                                                  │
│        enforcer:         Arc<Enforcer>,          ← fanotify      │
│      }                                                           │
└──────────────────────────────────────────────────────────────────┘
```

### 2.2 Existing Lock Query Interface

```rust
// lock_manager.rs — existing interface
pub fn is_file_locked(&self, file_path: &str) -> Result<bool, ErgataiError>
pub fn is_file_locked_for_write(&self, file_path: &str) -> Result<bool, ErgataiError>
pub fn record_violation(&self, file_path: &str, action: &str) -> Result<(), ErgataiError>
pub fn log_audit(&self, agent_id, session_id, action, file_path, mode, reason) -> Result<(), ErgataiError>
```

**Key issue**: `is_file_locked_for_write()` only returns `bool`, not "who locked it". When fanotify denies access, it needs to tell the user "file is locked by agent-X", so we need to extend the query interface.

### 2.3 PID → agent_id Mapping

`RmuxBackend::discover_agents()` (`crates/ergatai-runtime/src/backends/rmux.rs:1246`) already has a robust PID discovery mechanism:

```rust
// Already exists
fn read_proc_environ(pid: u32, var_name: &str) -> Option<String>
fn find_opencode_child_environ(pid: u32, var_name: &str) -> Option<String>
```

Discovery flow:
1. `rmux.find_panes().all()` → get all panes
2. Extract PID from `PaneProcessState::Running { pid }`
3. Read `/proc/{pid}/environ` to get `RMUX_PANE` (deterministic ID)
4. Walk `/proc/{pid}/task/{pid}/children` to find opencode child processes
5. Read `ERGATAI_AGENT_ID` environment variable

**Problem**: These functions are private methods of `RmuxBackend`. The fanotify enforcer also needs them, so they need to be promoted to a shared location.

### 2.4 AgentRegistry (Runtime)

`AgentRuntime` maintains `registry: Arc<RwLock<HashMap<String, AgentInfo>>>`, where:
- `AgentInfo.agent_id` — e.g., `%15` (from RMUX_PANE)
- `AgentInfo.handle.process_id: Option<String>` — child process PID (as string)
- `AgentInfo.handle.metadata["rmux_pane"]` — same as agent_id
- `AgentInfo.handle.metadata["ergatai_agent_id"]` — if set

---

## 3. Fanotify Technical Approach

### 3.1 Linux fanotify Mechanism

fanotify is a file system notification mechanism available since Linux 2.6.36+. Key features:

```
┌─────────────────────────────────────────────────────────────────┐
│  Application                                                     │
│    │                                                              │
│    │ 1. fanotify_init(FAN_CLASS_NOTIF | FAN_NONBLOCK, O_RDONLY) │
│    │ 2. fanotify_mark(fd, FAN_MARK_ADD,                          │
│    │                   FAN_OPEN_PERM | FAN_CLOSE_WRITE,         │
│    │                   AT_FDCWD, "/project/root")               │
│    │                                                              │
│    ▼                                                              │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Kernel fanotify subsystem                              │    │
│  │  ─────────────────────────────────────────────────────  │    │
│  │  Whenever a process open("/project/root/foo.rs", O_WRONLY):│
│  │    ① Kernel suspends the process (enters D state)        │    │
│  │    ② Generates FAN_OPEN_PERM event                       │    │
│  │    ③ Writes to fanotify fd                               │    │
│  │    ④ Waits for userspace response                        │    │
│  │    ⑤ Userspace writes back FAN_ALLOW or FAN_DENY         │    │
│  │    ⑥ Kernel resumes/rejects the original open() call     │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

**Key event types**:

| Event | Purpose | Blocking semantics |
|-------|---------|-------------------|
| `FAN_OPEN_PERM` | Intercept open permission request | Synchronous blocking, must respond |
| `FAN_ACCESS_PERM` | Intercept read permission (optional) | Synchronous blocking |
| `FAN_OPEN` | File open notification (non-permission) | Asynchronous |
| `FAN_CLOSE_WRITE` | Writable fd closed | Asynchronous |

We primarily use `FAN_OPEN_PERM` to intercept **write intent** (by checking the `open()` flags).

### 3.2 Detecting Write Operations

fanotify's `FAN_OPEN_PERM` event **does not directly expose open flags** (O_RDONLY/O_WRONLY/O_RDWR). Solutions:

**Approach A (Recommended): Use FAN_OPEN_PERM + /proc/{pid}/fdinfo**
```rust
// When receiving FAN_OPEN_PERM event:
// event.pid → PID of the triggering process
// event.fa.fid → file handle (requires FAN_REPORT_FID)
//
// Cannot see pending open via /proc/{pid}/fd/ or /proc/{pid}/maps
// But can use openat2() RESOLVE_NO_SYMLINKS etc. as auxiliary
//
// FAN_OPEN_PERM event itself doesn't contain flags, but we can
// check if the process already holds a write fd for the file to infer intent
// Simpler approach: intercept all open permission requests, use eBPF/audit for flags
```

**Approach B (More practical): Use FAN_OPEN_PERM + default deny for unknown writers**

Since fanotify permission events cannot directly distinguish read/write intent, a **two-stage detection** is recommended:

```
Stage 1: FAN_OPEN_PERM
  └─ Check if (pid, inode) is already marked as "pending write check"
  └─ Look up pid → agent_id → check lock table
  └─ If agent doesn't have WRITE/ADMIN lock on the file → DENY

Stage 2: FAN_CLOSE_WRITE
  └─ Record audit event when file is closed after modification
```

**Approach C (Most accurate, recommended for final): Use `FAN_RENAME` + `FAN_CREATE` + `FAN_DELETE` + `FAN_MODIFY`**

```rust
// Linux 5.17+ supports FAN_RENAME, FAN_CREATE, FAN_DELETE_SELF
// Combined with FAN_REPORT_FID, FAN_REPORT_TARGET_FID
// Can precisely capture mutation operations
```

**Actually adopted approach**: **Combination of A + B** —

```rust
// Monitor FAN_OPEN_PERM → synchronous interception
// Identify via /proc/{pid}/cmdline + pid → agent_id mapping
// Check /proc/{pid}/fd/ to see if process already holds rw fd for the file
//   (if process already holds rw fd, it was allowed at open time, subsequent writes pass)
// Otherwise check lock table
// If process is not in agent registry → ALLOW directly (non-agent processes are unmanaged)
```

### 3.3 Rust Crate Selection

```toml
# Cargo.toml (ergatai-lock)
[target.'cfg(target_os = "linux")'.dependencies]
fanotify-rs = "0.3.1"        # High-level fanotify API
fanotify-fid = "0.7.0"       # FID mode event parsing

# Or use nix crate directly (already an indirect dependency)
nix = { version = "0.28", features = ["fanotify", "process"] }
```

**Recommendation**: Use the `nix` crate's fanotify wrapper directly (lower-level but more controllable), or hand-write syscall wrappers. `fanotify-rs` 0.3.1 is relatively young and may lack modern features like `FAN_REPORT_FID`.

### 3.4 Performance Considerations

fanotify permission events are **synchronous blocking** — the intercepted process sleeps in the kernel until userspace responds. Key performance points:

| Stage | Latency budget | Notes |
|-------|---------------|-------|
| Event from kernel to userspace | < 1μs | fd read, zero-copy |
| PID → agent_id lookup | < 5μs | In-memory HashMap + cache |
| Lock table query | < 50μs | SQLite WAL read (hot path in memory cache) |
| Response write back to kernel | < 1μs | fd write |
| **Total latency** | **< 100μs** | Nearly imperceptible to agents |

**Optimization strategies**:
1. **Hot-path caching**: Maintain LRU cache of `HashMap<(pid, inode_hash), (agent_id, lock_expires_at)>`
2. **Batch responses**: Read multiple events in one `read()`, process in batch
3. **Allowlist**: Directly ALLOW ergatai's own processes (e.g., snapshot, watchdog)
4. **Non-agent processes**: Directly ALLOW PIDs not in registry (don't interfere with other system processes)

---

## 4. Architecture Design

### 4.1 Overall Architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│  ergatai server (main.rs)                                                │
│                                                                          │
│  ┌──────────────────┐   ┌──────────────────┐   ┌──────────────────────┐ │
│  │  AgentRuntime    │   │  FileLockManager │   │  Enforcer            │ │
│  │  ────────────    │   │  ─────────────── │   │  ─────────           │ │
│  │  registry:       │   │  SQLite locks.db │   │  fanotify fd         │ │
│  │    agent_id →    │   │  audit_log       │   │  pid → agent_id map  │ │
│  │    AgentInfo     │   │  tokens          │   │  inode → path cache  │ │
│  │    .process_id   │   │                  │   │  LRU decision cache  │ │
│  │    .metadata{}   │   │                  │   │                      │ │
│  └────────┬─────────┘   └────────▲─────────┘   └──────────┬───────────┘ │
│           │                      │                        │             │
│           │  get_agent_by_pid()  │  is_file_locked_by()   │             │
│           ├─────────────────────►│                        │             │
│           │                      │                        │             │
│           │                      │                        │             │
│           │                      │                        │             │
│           │                      │    ┌───────────────────┘             │
│           │                      │    │ read() events                   │
│           │                      │    ▼                                 │
│           │                      │  ┌───────────────────────┐           │
│           │                      │  │  Enforcer event loop  │           │
│           │                      │  │  ────────────────────  │           │
│           │                      │  │  pid → agent_id       │           │
│           │                      │  │  agent_id → locks?    │           │
│           │                      │  │  ALLOW / DENY         │           │
│           │                      │  │  → audit_log          │           │
│           │                      │  │  → NATS event         │           │
│           │                      │  └───────────────────────┘           │
│           │                      │                                      │
└──────────────────────────────────────────────────────────────────────────┘
```

### 4.2 New Module: `enforcer.rs`

```
crates/ergatai-lock/src/
├── enforcer.rs           ← New: fanotify mandatory lock implementation
├── pid_resolver.rs       ← New: PID → agent_id resolution (extracted from RmuxBackend)
├── lock_manager.rs       ← Modified: add is_file_locked_by() and other query methods
├── watcher.rs            ← Kept: detect-after-the-fact as fallback + non-Linux platforms
└── manager.rs            ← Modified: integrate Enforcer into ProjectFileAccess
```

### 4.3 Data Structure Design

```rust
// enforcer.rs

/// Linux fanotify-based file access enforcer.
///
/// Intercepts file open() calls at the kernel level and denies write
/// access to files that are not locked by the calling agent.
///
/// # Lifecycle
/// 1. Created during `init_file_access()` (after FileLockManager)
/// 2. Background task spawned on `start()`
/// 3. Stopped during `shutdown_file_access()`
///
/// # Failure Modes
/// - fanotify fd creation fails → fail-open (degrades to FileSystemWatcher mode)
/// - enforcer thread panics → fail-open (log + continue)
/// - Lock DB unavailable → fail-open (allow all access, log warning)
pub struct Enforcer {
    /// fanotify file descriptor (wrapped for safe drop)
    fanotify_fd: OwnedFd,
    /// Project root being monitored
    project_root: PathBuf,
    /// Lock manager for checking lock state
    lock_manager: Arc<FileLockManager>,
    /// Agent runtime for PID → agent_id resolution
    pid_resolver: Arc<PidResolver>,
    /// Hot-path decision cache: (pid, file_inode) → decision
    decision_cache: Arc<RwLock<LruCache<(u32, u64), CachedDecision>>>,
    /// Shutdown signal
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Background task handle
    task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Configuration
    config: EnforcerConfig,
    /// Metrics
    metrics: Arc<EnforcerMetrics>,
    /// Failure mode: if true, deny-all on uncertainty; if false, allow-all
    fail_closed: bool,
}

/// Configuration for the enforcer
#[derive(Debug, Clone)]
pub struct EnforcerConfig {
    /// Whether the enforcer is enabled at all
    pub enabled: bool,
    /// Paths to monitor (relative to project_root; empty = monitor all)
    pub watch_paths: Vec<PathBuf>,
    /// Paths to exclude (e.g., .git, .ergatai, node_modules)
    pub exclude_paths: Vec<PathBuf>,
    /// PIDs that are always allowed (ergatai's own processes)
    pub allowlist_pids: Vec<u32>,
    /// Whether to fail-closed (deny on uncertainty) vs fail-open
    pub fail_closed: bool,
    /// Decision cache size (LRU entries)
    pub cache_size: usize,
    /// Decision cache TTL (seconds)
    pub cache_ttl_secs: u64,
    /// Maximum permission response latency before warn-log (μs)
    pub latency_warn_threshold_us: u64,
}

impl Default for EnforcerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            watch_paths: vec![],  // empty = whole project_root
            exclude_paths: vec![
                ".git".into(),
                ".ergatai".into(),
                "node_modules".into(),
                "target".into(),
            ],
            allowlist_pids: vec![std::process::id()],  // self
            fail_closed: false,  // fail-open by default
            cache_size: 10_000,
            cache_ttl_secs: 5,
            latency_warn_threshold_us: 1000,  // 1ms
        }
    }
}

/// Cached decision for hot path
#[derive(Debug, Clone)]
struct CachedDecision {
    agent_id: String,
    session_id: String,
    allowed: bool,
    cached_at: Instant,
}

/// Metrics for observability
#[derive(Debug, Default)]
pub struct EnforcerMetrics {
    pub total_events: AtomicU64,
    pub allowed: AtomicU64,
    pub denied: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub errors: AtomicU64,
    pub avg_latency_us: AtomicU64,  // EWMA
}
```

### 4.4 PID Resolver

```rust
// pid_resolver.rs

/// Resolves Linux process PIDs to ergatai agent IDs.
///
/// Maintains a PID → agent_id mapping synchronized with AgentRuntime discovery.
/// Also resolves descendant processes (children, grandchildren) to their
/// ancestor agent.
///
/// This is extracted from RmuxBackend's private methods to be shared with
/// the fanotify enforcer.
pub struct PidResolver {
    /// PID → agent_id mapping (includes agent's own PID and all descendants)
    pid_to_agent: Arc<RwLock<HashMap<u32, AgentProcessInfo>>>,
    /// Reference to AgentRuntime for refreshing
    runtime: Arc<AgentRuntime>,
    /// Last refresh timestamp
    last_refresh: Arc<Mutex<Instant>>,
    /// Refresh interval
    refresh_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct AgentProcessInfo {
    pub agent_id: String,
    pub session_id: String,
    pub root_pid: u32,       // The agent's main process PID
    pub is_descendant: bool, // true if this PID is a child of the agent
}

impl PidResolver {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self { ... }

    /// Resolve a PID to an agent ID.
    /// Returns None if the PID does not belong to any known agent.
    pub async fn resolve(&self, pid: u32) -> Option<AgentProcessInfo> {
        // 1. Check cache
        {
            let map = self.pid_to_agent.read().await;
            if let Some(info) = map.get(&pid) {
                return Some(info.clone());
            }
        }

        // 2. Walk up the process tree to find an ancestor in the map
        //    (handles agents spawning shell commands)
        self.walk_ancestors(pid).await
    }

    /// Refresh the PID map from AgentRuntime registry.
    /// Called periodically and on-demand when cache misses spike.
    pub async fn refresh(&self) -> ErgataiResult<()> {
        let agents = self.runtime.list_agents().await;
        let mut map = self.pid_to_agent.write().await;
        map.clear();

        for agent in agents {
            if let Some(pid_str) = &agent.handle.process_id {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    // Register the agent's main PID
                    map.insert(pid, AgentProcessInfo {
                        agent_id: agent.agent_id.clone(),
                        session_id: agent.workspace_id.clone(),
                        root_pid: pid,
                        is_descendant: false,
                    });

                    // Enumerate all descendant PIDs
                    for descendant_pid in Self::enumerate_descendants(pid) {
                        map.insert(descendant_pid, AgentProcessInfo {
                            agent_id: agent.agent_id.clone(),
                            session_id: agent.workspace_id.clone(),
                            root_pid: pid,
                            is_descendant: true,
                        });
                    }
                }
            }
        }

        *self.last_refresh.lock().await = Instant::now();
        Ok(())
    }

    /// Walk /proc/{pid}/status PPid chain up to find a known ancestor.
    async fn walk_ancestors(&self, pid: u32) -> Option<AgentProcessInfo> {
        let mut current = pid;
        for _ in 0..32 {  // max depth
            let ppid = Self::read_ppid(current)?;
            if ppid == 0 || ppid == 1 { return None; }  // init

            let map = self.pid_to_agent.read().await;
            if let Some(info) = map.get(&ppid) {
                // Found! Also cache the original PID for next time
                drop(map);
                let mut map = self.pid_to_agent.write().await;
                map.insert(pid, info.clone());
                return Some(info.clone());
            }
            current = ppid;
        }
        None
    }

    /// Read parent PID from /proc/{pid}/status
    fn read_ppid(pid: u32) -> Option<u32> {
        let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        status.lines()
            .find(|l| l.starts_with("PPid:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
    }

    /// Recursively enumerate all descendant PIDs via /proc/{pid}/task/{pid}/children
    fn enumerate_descendants(pid: u32) -> Vec<u32> {
        let mut result = Vec::new();
        let children_path = format!("/proc/{}/task/{}/children", pid, pid);
        if let Ok(data) = std::fs::read_to_string(&children_path) {
            for child_str in data.split_whitespace() {
                if let Ok(child_pid) = child_str.parse::<u32>() {
                    result.push(child_pid);
                    result.extend(Self::enumerate_descendants(child_pid));
                }
            }
        }
        result
    }
}
```

### 4.5 LockManager Extension

```rust
// lock_manager.rs — new methods

/// Get the lock holder for a file (for write-mode locks).
/// Returns (agent_id, session_id) if a WRITE/ADMIN lock exists.
pub fn get_write_lock_holder(&self, file_path: &str) -> Result<Option<(String, String)>, ErgataiError> {
    let normalized = self.validate_and_normalize_path(file_path)
        .unwrap_or_else(|_| file_path.to_string());
    let conn = self.conn.lock()
        .map_err(|e| ErgataiError::internal(format!("lock: {}", e)))?;

    let mut stmt = conn.prepare_cached(
        "SELECT agent_id, session_id FROM file_locks
         WHERE file_path = ?1
           AND mode IN ('WRITE', 'ADMIN')
           AND status = 'ACTIVE'
         LIMIT 1"
    )?;

    let result = stmt.query_row(params![normalized], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });

    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(ErgataiError::internal(format!("query: {}", e))),
    }
}

/// Record a fanotify-enforced violation with known agent identity.
pub fn record_enforced_violation(
    &self,
    agent_id: &str,
    session_id: &str,
    file_path: &str,
    attempted_mode: &str,  // "WRITE" / "ADMIN"
    lock_holder: Option<(&str, &str)>,  // (holder_agent, holder_session) if any
) -> Result<(), ErgataiError> {
    let action = if lock_holder.is_some() {
        "ENFORCED_WRITE_CONFLICT"
    } else {
        "ENFORCED_WRITE_WITHOUT_LOCK"
    };

    let reason = if let Some((holder_agent, _)) = lock_holder {
        format!("file locked by {}", holder_agent)
    } else {
        "no active lock held by this agent".to_string()
    };

    self.log_audit(
        agent_id,
        session_id,
        action,
        Some(file_path),
        Some(attempted_mode),
        Some(&reason),
    )
}
```

### 4.6 Integration into FileAccessManager

```rust
// manager.rs — modified ProjectFileAccess

struct ProjectFileAccess {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
    enforcer: Option<Arc<Enforcer>>,  // ← New, None if disabled/unsupported
}

pub async fn init_file_access(project_id: &str, project_root: &Path) -> ErgataiResult<()> {
    // ... existing code ...

    // Create Enforcer (Linux only, fail-open degradation on failure)
    let enforcer = match Enforcer::new(
        lock_manager.clone(),
        project_root.to_path_buf(),
        get_agent_runtime(),
        EnforcerConfig::default(),
    ) {
        Ok(enc) => {
            let enc = Arc::new(enc);
            if let Err(e) = enc.start().await {
                warn!(error = %e, "Failed to start enforcer, falling back to advisory mode");
                None
            } else {
                info!("fanotify enforcer started for project {}", project_id);
                Some(enc)
            }
        }
        Err(e) => {
            warn!(error = %e, "fanotify enforcer unavailable, using advisory mode");
            None
        }
    };

    manager.projects.insert(project_id.to_string(), ProjectFileAccess {
        lock_manager,
        snapshot_manager,
        watchdog,
        enforcer,
    });

    Ok(())
}

pub async fn shutdown_file_access(project_id: &str) -> ErgataiResult<()> {
    // ...
    if let Some(project) = manager.projects.remove(project_id) {
        project.lock_manager.shutdown_nats_subscription();

        // Stop enforcer first (so it doesn't query a dead lock manager)
        if let Some(enforcer) = project.enforcer {
            enforcer.stop().await;
        }

        let mut watchdog = project.watchdog.write().await;
        watchdog.stop()?;
    }
    Ok(())
}
```

---

## 5. Event Loop Core Logic

### 5.1 Main Loop Pseudocode

```rust
impl Enforcer {
    async fn event_loop(self: Arc<Self>) {
        let mut shutdown_rx = self.shutdown_rx.take().unwrap();
        let mut buf = vec![0u8; 4096];  // fanotify event buffer

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("Enforcer shutting down");
                    break;
                }

                // fanotify fd is non-blocking, use AsyncFd
                result = self.async_fd.readable() => {
                    match result {
                        Ok(guard) => {
                            match guard.try_io(|fd| {
                                Self::read_events_from_fd(fd.get_ref(), &mut buf)
                            }) {
                                Ok(Ok(events)) => {
                                    self.handle_events(events).await;
                                }
                                Ok(Err(e)) => {
                                    warn!(error = %e, "fanotify read error");
                                    self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(_would_block) => {
                                    // Spurious wakeup, continue
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "AsyncFd error");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle_events(&self, events: Vec<FanotifyPermissionEvent>) {
        for event in events {
            let start = Instant::now();

            let decision = self.decide(&event).await;

            // Write response back to kernel
            if let Err(e) = self.write_response(event.metadata.id, decision.allowed) {
                error!(error = %e, "Failed to write fanotify response");
            }

            // Record metrics + audit (async, non-blocking the response)
            let latency = start.elapsed();
            self.record_metrics(decision, latency);

            if !decision.allowed {
                // Log to audit + publish NATS event (fire-and-forget)
                let _ = self.log_denial(&event, &decision).await;
            }

            // Warn on slow responses
            if latency.as_micros() as u64 > self.config.latency_warn_threshold_us {
                warn!(
                    latency_us = latency.as_micros() as u64,
                    pid = event.pid,
                    "Slow fanotify response"
                );
            }
        }
    }

    async fn decide(&self, event: &FanotifyPermissionEvent) -> Decision {
        let pid = event.pid;

        // 1. Check if PID is in allowlist (ergatai's own processes)
        if self.config.allowlist_pids.contains(&pid) {
            return Decision { allowed: true, reason: "allowlisted".into() };
        }

        // 2. Check decision cache
        {
            let cache = self.decision_cache.read().await;
            if let Some(cached) = cache.get(&(pid, event.file_inode)) {
                if cached.cached_at.elapsed() < Duration::from_secs(self.config.cache_ttl_secs) {
                    self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
                    return Decision {
                        allowed: cached.allowed,
                        reason: format!("cached:{}", cached.agent_id),
                    };
                }
            }
        }
        self.metrics.cache_misses.fetch_add(1, Ordering::Relaxed);

        // 3. Resolve PID → agent_id
        let agent_info = match self.pid_resolver.resolve(pid).await {
            Some(info) => info,
            None => {
                // Unknown PID — not an agent's process
                // Default policy: allow (don't interfere with system processes)
                return Decision {
                    allowed: !self.fail_closed,
                    reason: "unknown_pid".into(),
                };
            }
        };

        // 4. Resolve file path from inode
        let file_path = match self.resolve_file_path(event) {
            Some(p) => p,
            None => {
                warn!(inode = event.file_inode, "Cannot resolve file path from inode");
                return Decision {
                    allowed: !self.fail_closed,
                    reason: "unresolvable_path".into(),
                };
            }
        };

        // 5. Check if path is under excluded dirs
        if self.is_excluded(&file_path) {
            return Decision { allowed: true, reason: "excluded_path".into() };
        }

        // 6. Check if this is a write intent
        //    (This is the tricky part — see §3.4 for discussion)
        //    Heuristic: check if the file's inode has been opened for writing
        //    via /proc/{pid}/fd/* inspection. But this has TOCTOU issues.
        //
        //    Better approach: intercept ALL opens and check if agent has
        //    ANY lock. Reads are always allowed if agent has READ/READ_LATEST.
        //    Writes require WRITE/ADMIN.
        //
        //    Even better: use FAN_OPEN_PERM + FAN_CLOSE_WRITE together.
        //    FAN_OPEN_PERM is always answered ALLOW.
        //    FAN_CLOSE_WRITE triggers the lock check and records violation
        //    if no lock held.
        //
        //    For TRUE enforcement, we need FAN_OPEN_PERM to inspect
        //    /proc/{pid}/fdinfo/{fd} after the open completes — but
        //    that's not available at permission-check time.
        //
        //    Recommended approach: use eBPF supplement OR accept that
        //    fanotify alone cannot distinguish read/write open flags,
        //    and use a hybrid model:
        //      - FAN_OPEN_PERM always ALLOW (but record the open)
        //      - FAN_CLOSE_WRITE → check lock → if violated, trigger
        //        remediation (e.g., SIGSTOP the process, log, notify)
        //
        //    Since fanotify can't distinguish read/write at open time,
        //    we use a TWO-TIER approach:
        //      Tier 1 (FAN_CLOSE_WRITE): Detect writes that bypassed locks
        //      Tier 2 (LSM/eBPF supplement): Optional future enhancement

        // 7. For now, use a practical approximation:
        //    Check if agent has ANY lock on the file. If not, check if
        //    the file is locked by anyone else. If locked by others → DENY.
        //    This prevents concurrent write conflicts but doesn't prevent
        //    "read + modify + write" bypass (which needs eBPF supplement).
        let file_path_str = file_path.to_string_lossy();

        match self.lock_manager.get_write_lock_holder(&file_path_str) {
            Ok(Some((holder_agent, holder_session))) => {
                // File is locked by someone
                if holder_agent == agent_info.agent_id {
                    // Same agent — ALLOW (it's the lock holder)
                    Decision {
                        allowed: true,
                        reason: format!("self_lock:{}", holder_agent),
                    }
                } else {
                    // Different agent — DENY
                    Decision {
                        allowed: false,
                        reason: format!("locked_by:{}", holder_agent),
                    }
                }
            }
            Ok(None) => {
                // File is not locked — ALLOW (no conflict)
                // Note: This means "unlocked writes" are still allowed.
                // To enforce "must hold lock to write", we'd need
                // FAN_CLOSE_WRITE detection or eBPF.
                Decision {
                    allowed: true,
                    reason: "unlocked".into(),
                }
            }
            Err(e) => {
                error!(error = %e, "Lock query failed");
                Decision {
                    allowed: !self.fail_closed,
                    reason: "error".into(),
                }
            }
        }
    }
}
```

### 5.2 Key Challenge: Read/Write Intent Distinction

fanotify `FAN_OPEN_PERM` events **do not expose open flags**. This is the core challenge.

**Solution evolution**:

| Approach | Implementation complexity | Accuracy | Recommendation |
|----------|--------------------------|----------|----------------|
| A: Only intercept WRITE lock holder conflicts | Low | Medium (prevents concurrent conflicts, not unlocked writes) | ★★★★ Short-term |
| B: FAN_CLOSE_WRITE post-hoc remediation | Low | Medium (detects after write, can rollback) | ★★★ Auxiliary |
| C: eBPF supplement (bpf_openfile) | High | High (can read flags precisely) | ★★★★★ Long-term |
| D: Custom LSM module | Very high | Highest | ★★ Kernel-level |

**Recommended phased implementation**:

**Phase 1 (MVP, focus of this document)**:
```
FAN_OPEN_PERM + /proc/{pid}/fd/* inspection + process tree analysis
↓
Interception scenario: agent A holds WRITE lock on file.rs
             agent B (via shell `echo >> file.rs`) open(file.rs, O_WRONLY)
             → Kernel suspends B's open()
             → Enforcer queries lock table → finds holder = A
             → Returns FAN_DENY → B's open() fails with EPERM
             → Audit: "ENFORCED_WRITE_CONFLICT agent=B holder=A"
```

**Phase 2 (Enhancement)**:
```
FAN_CLOSE_WRITE + file hash comparison (similar to existing FileSystemWatcher)
↓
Detection: file modified without lock → trigger SIGSTOP + notify + auto-rollback (git checkout)
```

**Phase 3 (Ultimate)**:
```
eBPF bpf_openfile program intercepts openat2() system calls
↓
Precisely reads open flags, distinguishes O_RDONLY / O_WRONLY / O_RDWR
Determines intent at open entry point, no /proc secondary query needed
```

### 5.3 Practical Approach for Handling Open Flags

Since fanotify cannot directly read open flags, we use a **hybrid strategy**:

```rust
/// For FAN_OPEN_PERM events, we try the following methods to infer open intent:
///
/// 1. /proc/{pid}/cmdline: Check the command the process is executing
///    - If cmdline contains "cat", "less", "head" → likely READ
///    - If cmdline contains "vim", "sed -i", "echo >>" → likely WRITE
///    - Unreliable (processes can rename, cmdline changes after exec)
///
/// 2. /proc/{pid}/fd/* scan (at FAN_OPEN_PERM time):
///    - List fds held by the process, find the just-opened one (latest inode match)
///    - Read /proc/{pid}/fdinfo/{fd} to get open flags
///    - Problem: at FAN_OPEN_PERM time, open hasn't completed, fd doesn't exist yet
///
/// 3. Most feasible in practice: Use FAN_CLOSE_WRITE + hash comparison:
///    - Record pre-hash for each monitored file (at FAN_OPEN time)
///    - Compare post-hash when FAN_CLOSE_WRITE triggers
///    - If hash changed and no lock → violation → rollback (git checkout) + audit
///
/// 4. Or use fanotify's FAN_MODIFY event:
///    - FAN_MODIFY triggers on pwrite/write (non-permission event)
///    - Can combine with FAN_CLOSE_WRITE for "detect modification"
///
/// 5. Best compromise: Intercept all FAN_OPEN_PERM, perform lock check
///    for "possibly write" opens, ALLOW the rest:
///    - If file has WRITE/ADMIN lock and caller is not holder → DENY
///    - If file has no lock → ALLOW (but still monitorable via FAN_CLOSE_WRITE)
///    - This at least prevents "two agents writing same file simultaneously" conflicts
```

---

## 6. NATS Event Integration

### 6.1 New Event Type

```rust
// ergatai-nats/src/events.rs — new

/// Published when the fanotify enforcer denies a file access
pub struct FileAccessEnforcedPayload {
    /// Timestamp
    pub timestamp: String,
    /// Agent that was denied
    pub agent_id: String,
    /// Agent's session
    pub session_id: String,
    /// Process PID that triggered the event
    pub pid: u32,
    /// File path that was denied
    pub file_path: String,
    /// What the agent was trying to do ("OPEN_FOR_WRITE", "MODIFY")
    pub attempted_action: String,
    /// Who holds the lock (if any)
    pub lock_holder_agent_id: Option<String>,
    /// Lock holder's session
    pub lock_holder_session_id: Option<String>,
    /// Enforcer decision reason
    pub reason: String,
}

// Subject: ergatai.file.enforced.{project_id}
// Stream: FILE_EVENTS (existing)
```

### 6.2 Publish Logic

```rust
impl Enforcer {
    async fn publish_enforcement_event(&self, event: &FanotifyEvent, decision: &Decision) {
        let Some(nats) = get_nats_connection().await else { return };

        let payload = FileAccessEnforcedPayload {
            timestamp: Utc::now().to_rfc3339(),
            agent_id: decision.agent_id.clone().unwrap_or_default(),
            session_id: decision.session_id.clone().unwrap_or_default(),
            pid: event.pid,
            file_path: event.file_path.clone(),
            attempted_action: "OPEN_FOR_WRITE".to_string(),
            lock_holder_agent_id: decision.lock_holder.clone().map(|(a, _)| a),
            lock_holder_session_id: decision.lock_holder.clone().map(|(_, s)| s),
            reason: decision.reason.clone(),
        };

        let subject = format!("ergatai.file.enforced.{}", self.project_id);
        let data = serde_json::to_vec(&payload).unwrap_or_default();

        if let Err(e) = nats.client().publish(subject, data.into()).await {
            warn!(error = %e, "Failed to publish enforcement event");
        }
    }
}
```

---

## 7. Integration with AgentRuntime

### 7.1 PID Registration Hook

After `RmuxBackend::discover_agents()` completes, notify PidResolver to refresh:

```rust
// runtime.rs — AgentRuntime

pub async fn discover_and_register_agents(self: &Arc<Self>) -> ErgataiResult<usize> {
    let discovered = self.backend.discover_agents().await?;
    let mut new_count = 0;

    {
        let mut registry = self.registry.write().await;
        for (agent_id, handle) in discovered {
            if !registry.contains_key(&agent_id) {
                // ... existing logic ...
                new_count += 1;
            }
        }
    }

    // ✨ New: notify PidResolver to refresh
    if new_count > 0 {
        if let Some(resolver) = get_pid_resolver() {
            resolver.refresh().await?;
        }
    }

    Ok(new_count)
}
```

### 7.2 Global PidResolver

```rust
// pid_resolver.rs — global singleton

static PID_RESOLVER: OnceLock<Arc<PidResolver>> = OnceLock::new();

pub fn init_pid_resolver(runtime: Arc<AgentRuntime>) -> ErgataiResult<Arc<PidResolver>> {
    let resolver = Arc::new(PidResolver::new(runtime));
    PID_RESOLVER.set(resolver.clone())
        .map_err(|_| ErgataiError::internal("PidResolver already initialized"))?;
    Ok(resolver)
}

pub fn get_pid_resolver() -> Option<Arc<PidResolver>> {
    PID_RESOLVER.get().cloned()
}
```

---

## 8. Testing Strategy

### 8.1 Unit Tests (no root required)

```rust
#[cfg(test)]
mod tests {
    // Test PidResolver's /proc parsing logic
    // Test decision cache LRU behavior
    // Test EnforcerConfig's exclude_paths matching
    // Test lock_manager.get_write_lock_holder()

    #[tokio::test]
    async fn test_pid_resolver_walks_ancestors() {
        // Fork a child process in the test
        // Register parent process as agent-1
        // Verify child's resolve() returns agent-1
    }

    #[tokio::test]
    async fn test_decision_cache_expires() {
        // Cache a decision, wait for TTL to expire
        // Verify cache miss
    }
}
```

### 8.2 Integration Tests (requires Linux + root)

```rust
#[cfg(all(test, target_os = "linux"))]
mod integration_tests {
    // Need #[ignore] by default, run manually
    // Or detect CAP_SYS_ADMIN capability

    #[tokio::test]
    #[ignore]  // requires root
    async fn test_enforcer_denies_unauthorized_write() {
        // 1. Start enforcer on temp dir
        // 2. agent-1 acquires WRITE lock on "file.txt"
        // 3. Simulate agent-2's process open("file.txt", O_WRONLY)
        // 4. Verify open() returns EPERM
        // 5. Verify audit_log contains ENFORCED_WRITE_CONFLICT
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_allows_lock_holder() {
        // agent-1 holds lock → agent-1's process can open
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_allows_non_agent_process() {
        // Non-agent PIDs are not intercepted
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_fail_open_on_lock_db_error() {
        // Simulate SQLite error → verify fail-open
    }
}
```

### 8.3 CI Considerations

```yaml
# GitHub Actions
- name: fanotify integration tests
  if: runner.os == 'Linux'
  run: cargo test -p ergatai-lock -- --ignored fanotify
  # Requires --privileged or CAP_SYS_ADMIN
```

---

## 9. Risks and Limitations

### 9.1 Platform Limitations

| Issue | Impact | Mitigation |
|-------|--------|------------|
| Linux-only | macOS/Windows cannot use | `#[cfg(target_os = "linux")]`, keep FileSystemWatcher on other platforms |
| Requires root/CAP_SYS_ADMIN | Regular users cannot enable | Document; or setuid wrapper |
| Kernel version requirements | FAN_REPORT_FID needs 5.1+, FAN_RENAME needs 5.17+ | Detect kernel version, degrade to basic FAN_OPEN_PERM |

### 9.2 Compatibility

| Scenario | Behavior | Notes |
|----------|----------|-------|
| Docker container | fanotify available by default in containers | Needs `--privileged` or `CAP_SYS_ADMIN` |
| OverlayFS | FID mode may not be supported | Needs testing; can fall back to non-FID mode |
| NFS/network FS | fanotify not supported | Only monitor local paths |
| btrfs/ZFS | Should work | Recommend test coverage |
| Multiple mount namespaces | fanotify is per-mount | Need to mark each mount point |

### 9.3 Performance Impact

**Hot path analysis** (per file open):
```
Kernel → fanotify fd read:      ~500ns
Event parsing:                  ~200ns
PID cache lookup:               ~100ns (hit) / ~5μs (miss + refresh)
Lock table query (SQLite WAL):  ~5μs (cached) / ~50μs (disk)
Decision serialization:         ~100ns
Write back to kernel:           ~500ns
─────────────────────────────────────────
Total (cache hit):              ~2μs
Total (cache miss):             ~10μs
```

**Impact assessment**:
- Normal development workflow: no perceptible delay (<10μs per open)
- High-frequency I/O (e.g., cargo build): may add 1-5% total time
- Recommendation: exclude `target/` directory by default

### 9.4 Failure Modes

| Failure | Default behavior | Configurable |
|---------|-----------------|--------------|
| Enforcer panic | fail-open (allow all) | `fail_closed: true` to invert |
| SQLite unavailable | fail-open | `fail_closed: true` to invert |
| NATS unavailable | Continue working (local audit only) | ✓ |
| PidResolver stale | Reject new agents (until refresh) | Shorten refresh_interval |
| fanotify init fails | Degrade to FileSystemWatcher mode | ✓ |

### 9.5 Security Risks

| Risk | Mitigation |
|------|------------|
| Agent bypasses PID mapping via `su`/`sudo` to other user | Check /proc/{pid}/uid, non-agent uid → deny |
| Agent switches namespace via `nsenter` | fanotify is mount namespace isolated |
| Agent calls syscall directly (bypassing glibc) | fanotify intercepts at kernel level, cannot bypass |
| Agent modifies enforcer's own code | Via .ergatai/ exclusion + lock protection |

---

## 10. Implementation Roadmap

### Phase 1: MVP (2-3 weeks)
- [ ] Extract `pid_resolver.rs` from RmuxBackend
- [ ] Implement `enforcer.rs` basic framework (fanotify mark + event loop)
- [ ] Implement "holder conflict detection" (intercept writes already locked by another agent)
- [ ] Integrate into `manager.rs` + `lib.rs`
- [ ] Unit tests + manual integration tests
- [ ] NATS event publishing

### Phase 2: Enhancement (1-2 weeks)
- [ ] `FAN_CLOSE_WRITE` post-hoc remediation (unlocked write detection + auto git checkout rollback)
- [ ] `get_write_lock_holder()` interface
- [ ] Decision cache optimization
- [ ] `enforcer_metrics` observability (Prometheus metrics)
- [ ] Config hot-reload (watch config file)

### Phase 3: Precise Intent Detection (2-3 weeks)
- [ ] eBPF supplement (precisely read open flags)
- [ ] Or: use `seccomp-bpf` to intercept openat
- [ ] Distinguish READ / WRITE / CREATE / DELETE operations
- [ ] Implement "must hold lock to write" strict policy

### Phase 4: Production Hardening (ongoing)
- [ ] CI integration (privileged test runner)
- [ ] Performance benchmarking
- [ ] Documentation + user guide
- [ ] Gradual rollout (feature flag: `enforcer.enabled`)

---

## 11. Key Decision Records (ADR)

### ADR-1: Choose fanotify over inotify
**Decision**: Use fanotify
**Rationale**:
- inotify only provides post-hoc notifications (no permission events)
- fanotify provides FAN_OPEN_PERM synchronous interception
- fanotify supports global PID information (inotify only has wd)
- fanotify supports FAN_REPORT_FID (inode-based, no path watch needed)

### ADR-2: fail-open vs fail-closed default policy
**Decision**: Default fail-open
**Rationale**:
- fail-closed would block all development workflows when enforcer errors
- Ergatai is a development tool, not a security product
- Advanced users can configure `fail_closed: true` to switch

### ADR-3: Phased implementation vs all-at-once
**Decision**: Phased (Phase 1 does conflict detection, Phase 3 does precise intent)
**Rationale**:
- Phase 1 already solves the most serious "concurrent write conflict" problem
- eBPF integration is complex and requires kernel adaptation
- Gradual rollout reduces risk

### ADR-4: PidResolver as independent module
**Decision**: Extract from RmuxBackend as shared module
**Rationale**:
- fanotify enforcer needs the same PID → agent_id mapping
- May be used for other features in the future (resource isolation, network policy)
- Avoid coupling fanotify logic into RmuxBackend

---

## 12. Summary

This design provides **kernel-level mandatory file locking** for Ergatai based on Linux fanotify's `FAN_OPEN_PERM` permission events. Core innovations:

1. **PID → agent_id resolver**: Reuses RmuxBackend's existing `/proc` analysis logic
2. **Hot-path caching**: Ensures <10μs response latency
3. **fail-open policy**: Enforcer failure doesn't block development workflows
4. **Phased implementation**: Start with the most valuable conflict detection, then iterate

**Phase 1 coverage**:
- ✅ Prevents two agents from writing the same file simultaneously
- ✅ Tells users "file is locked by agent-X" (instead of silent failure)
- ✅ Audit log records all interception events
- ❌ Cannot prevent "unlocked writes" (Phase 2/3)
- ❌ Linux only, requires root/CAP_SYS_ADMIN

**Architecture compatibility**:
- Complements existing `FileSystemWatcher` (fanotify pre-interception + notify post-detection)
- Shares SQLite database with `FileLockManager`
- Publishes events via NATS for UI/dashboard consumption
