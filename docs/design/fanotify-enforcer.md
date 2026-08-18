# 基于 fanotify 的文件锁强制实现设计

> **Status**: Draft
> **Author**: fanotify-design agent
> **Created**: 2026-08-18
> **Related Crates**: `ergatai-lock`, `ergatai-runtime`, `ergatai-nats`

---

## 1. Executive Summary

Ergatai 当前的文件锁是纯咨询式（advisory）的：agent 通过 MCP 协议请求 `send_message` / acquire lock，但可以直接用 shell 命令（`echo x > file`、`sed -i`、`vim`）绕过。现有的 `FileSystemWatcher`（Phase 6）使用 `notify` crate 做 **事后检测**（detect-after-the-fact），只能记录违规、不能阻止。

本方案引入 **Linux fanotify** 的 `FAN_OPEN_PERM` 权限事件，在内核层拦截写操作的 `open()` 系统调用，实现真正的 **强制锁（mandatory locking）**。

### 1.1 核心价值

| 维度 | 现状 (advisory) | fanotify 方案 (mandatory) |
|------|----------------|--------------------------|
| 绕过难度 | 任意 shell 命令可绕过 | 内核层拦截，无 root 无法绕过 |
| 检测时机 | 文件被修改后（notify event） | 文件打开前（permission event） |
| 违规成本 | 事后审计 + 告警 | 操作直接失败（EACCES/EPERM） |
| Agent 体验 | 无感 | 收到明确的 "file locked by agent X" 错误 |

---

## 2. 现有架构分析

### 2.1 关键组件

```
┌──────────────────────────────────────────────────────────────────┐
│  FileAccessManager (manager.rs)                                  │
│  ─────────────────────────────────────────────────────────────── │
│  OnceLock<RwLock<HashMap<project_id, ProjectFileAccess>>>        │
│  └── ProjectFileAccess {                                         │
│        lock_manager:     Arc<FileLockManager>,   ← SQLite 锁库   │
│        snapshot_manager: Arc<SnapshotManager>,   ← Git COW      │
│        watchdog:         Arc<RwLock<Watchdog>>,  ← 心跳/过期    │
│        // ✨ 新增                                                 │
│        enforcer:         Arc<Enforcer>,          ← fanotify 强制 │
│      }                                                           │
└──────────────────────────────────────────────────────────────────┘
```

### 2.2 现有锁查询接口

```rust
// lock_manager.rs — 已有接口
pub fn is_file_locked(&self, file_path: &str) -> Result<bool, ErgataiError>
pub fn is_file_locked_for_write(&self, file_path: &str) -> Result<bool, ErgataiError>
pub fn record_violation(&self, file_path: &str, action: &str) -> Result<(), ErgataiError>
pub fn log_audit(&self, agent_id, session_id, action, file_path, mode, reason) -> Result<(), ErgataiError>
```

**关键问题**：`is_file_locked_for_write()` 只返回 `bool`，不返回 "谁锁了它"。fanotify 拒绝时需要告诉用户 "文件被 agent-X 锁定"，所以需要扩展一个查询接口。

### 2.3 PID → agent_id 映射

`RmuxBackend::discover_agents()`（`crates/ergatai-runtime/src/backends/rmux.rs:1246`）已经有完善的 PID 发现机制：

```rust
// 已存在
fn read_proc_environ(pid: u32, var_name: &str) -> Option<String>
fn find_opencode_child_environ(pid: u32, var_name: &str) -> Option<String>
```

发现流程：
1. `rmux.find_panes().all()` → 获取所有 pane
2. 从 `PaneProcessState::Running { pid }` 提取 PID
3. 读 `/proc/{pid}/environ` 获取 `RMUX_PANE`（确定性 ID）
4. 遍历 `/proc/{pid}/task/{pid}/children` 找 opencode 子进程
5. 读 `ERGATAI_AGENT_ID` 环境变量

**问题**：这些函数是 `RmuxBackend` 的私有方法。fanotify enforcer 也需要用，需要提升到共享位置。

### 2.4 AgentRegistry（运行时）

`AgentRuntime` 维护 `registry: Arc<RwLock<HashMap<String, AgentInfo>>>`，其中：
- `AgentInfo.agent_id` — 如 `%15`（来自 RMUX_PANE）
- `AgentInfo.handle.process_id: Option<String>` — 子进程 PID（字符串形式）
- `AgentInfo.handle.metadata["rmux_pane"]` — 与 agent_id 相同
- `AgentInfo.handle.metadata["ergatai_agent_id"]` — 如果设置

---

## 3. fanotify 技术方案

### 3.1 Linux fanotify 机制

fanotify 是 Linux 2.6.36+ 提供的文件系统通知机制，关键特性：

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
│  │  每当进程 open("/project/root/foo.rs", O_WRONLY) 时：    │    │
│  │    ① 内核暂停该进程（进入 D 状态）                        │    │
│  │    ② 生成 FAN_OPEN_PERM 事件                            │    │
│  │    ③ 写入 fanotify fd                                   │    │
│  │    ④ 等待用户态响应                                      │    │
│  │    ⑤ 用户态写回 FAN_ALLOW 或 FAN_DENY                    │    │
│  │    ⑥ 内核恢复/拒绝原进程的 open() 调用                   │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

**关键事件类型**：

| Event | 用途 | 阻塞语义 |
|-------|------|---------|
| `FAN_OPEN_PERM` | 拦截 open 权限请求 | 同步阻塞，必须响应 |
| `FAN_ACCESS_PERM` | 拦截 read 权限（可选） | 同步阻塞 |
| `FAN_OPEN` | 文件打开通知（非权限） | 异步 |
| `FAN_CLOSE_WRITE` | 可写 fd 关闭 | 异步 |

我们主要使用 `FAN_OPEN_PERM` 拦截 **写意图**（通过检查 `open()` 的 flags）。

### 3.2 检测写操作的 trick

fanotify 的 `FAN_OPEN_PERM` 事件 **不直接暴露 open flags**（O_RDONLY/O_WRONLY/O_RDWR）。解决方案：

**方案 A（推荐）: 使用 FAN_OPEN_PERM + /proc/{pid}/fdinfo**
```rust
// 收到 FAN_OPEN_PERM 事件时：
// event.pid → 触发进程的 PID
// event.fa.fid → 文件的 file handle (需要 FAN_REPORT_FID)
//
// 通过 /proc/{pid}/fd/ 或 /proc/{pid}/maps 无法看到未完成的 open
// 但可以通过 openat2() 的 RESOLVE_NO_SYMLINKS 等辅助
//
// 实际上 FAN_OPEN_PERM 事件本身不含 flags，但我们可以通过
// 检查进程是否已持有该文件的写 fd 来判断意图
// 更简单的做法：拦截所有 open 权限请求，用 eBPF/audit 补充 flags 信息
```

**方案 B（更实用）: 使用 `fanotify` 的 `FAN_OPEN_PERM` + 默认拒绝未知写者**

由于 fanotify permission events 无法直接区分 read/write intent，推荐 **两阶段检测**：

```
Stage 1: FAN_OPEN_PERM
  └─ 检查 (pid, inode) 是否已被本 enforcer 标记为 "pending write check"
  └─ 通过 pid 查 agent_id → 查 lock table
  └─ 如果 agent 没有该文件的 WRITE/ADMIN 锁 → 拒绝

Stage 2: FAN_CLOSE_WRITE
  └─ 文件被修改后关闭时记录审计事件
```

**方案 C（最准确，推荐最终方案）: 使用 `FAN_RENAME` + `FAN_CREATE` + `FAN_DELETE` + `FAN_MODIFY`**

```rust
// Linux 5.17+ 支持 FAN_RENAME, FAN_CREATE, FAN_DELETE_SELF
// 配合 FAN_REPORT_FID, FAN_REPORT_TARGET_FID
// 可以精确捕获 mutation 操作
```

**实际采用方案**: **方案 A + B 的组合** —

```rust
// 监听 FAN_OPEN_PERM → 同步拦截
// 通过 /proc/{pid}/cmdline + pid → agent_id 映射判断身份
// 通过 /proc/{pid}/fd/ 检查该进程是否已持有该文件的 rw fd
//   (如果进程已经持有 rw fd，说明它在 open 时已被放过，后续 write 直接允许)
// 否则检查 lock table
// 如果进程不在 agent registry 中 → 直接 ALLOW（非 agent 进程不受管控）
```

### 3.3 Rust crate 选择

```toml
# Cargo.toml (ergatai-lock)
[target.'cfg(target_os = "linux")'.dependencies]
fanotify-rs = "0.3.1"        # 高级 fanotify API
fanotify-fid = "0.7.0"       # FID 模式事件解析

# 或者直接使用 nix crate (已有间接依赖)
nix = { version = "0.28", features = ["fanotify", "process"] }
```

**推荐**: 直接使用 `nix` crate 的 fanotify 封装（更底层但更可控），或手写 syscall 包装。`fanotify-rs` 0.3.1 相对年轻，可能缺少 `FAN_REPORT_FID` 等现代特性。

### 3.4 性能考虑

fanotify 的 permission event 是 **同步阻塞** 的 — 被拦截的进程在内核中睡眠直到用户态响应。性能关键点：

| 环节 | 延迟预算 | 备注 |
|------|---------|------|
| 事件从内核到用户态 | < 1μs | fd read，零拷贝 |
| PID → agent_id 查询 | < 5μs | 内存 HashMap + 缓存 |
| lock table 查询 | < 50μs | SQLite WAL 读（内存缓存热路径）|
| 响应写回内核 | < 1μs | fd write |
| **总延迟** | **< 100μs** | 对 agent 几乎无感 |

**优化策略**：
1. **热路径缓存**: 维护 `HashMap<(pid, inode_hash), (agent_id, lock_expires_at)>` 的 LRU 缓存
2. **批量响应**: 一次 `read()` 读多个事件，批量处理
3. **白名单**: 对 ergatai 自身的进程（如 snapshot, watchdog）直接 ALLOW
4. **非 agent 进程**: 不在 registry 中的 PID 直接 ALLOW（不干扰系统其他进程）

---

## 4. 架构设计

### 4.1 总体架构

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

### 4.2 新增模块：`enforcer.rs`

```
crates/ergatai-lock/src/
├── enforcer.rs           ← 新增：fanotify 强制锁实现
├── pid_resolver.rs       ← 新增：PID → agent_id 解析（从 RmuxBackend 提升）
├── lock_manager.rs       ← 修改：新增 is_file_locked_by() 等查询方法
├── watcher.rs            ← 保留：事后检测作为 fallback + 非 Linux 平台
└── manager.rs            ← 修改：集成 Enforcer 到 ProjectFileAccess
```

### 4.3 数据结构设计

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
/// - fanotify fd 创建失败 → fail-open (降级到 FileSystemWatcher 模式)
/// - enforcer 线程 panic → fail-open (log + 继续)
/// - 锁库不可用 → fail-open (允许所有访问，记录 warning)
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

### 4.4 PID 解析器

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

### 4.5 LockManager 扩展

```rust
// lock_manager.rs — 新增方法

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

### 4.6 集成到 FileAccessManager

```rust
// manager.rs — 修改 ProjectFileAccess

struct ProjectFileAccess {
    lock_manager: Arc<FileLockManager>,
    snapshot_manager: Arc<SnapshotManager>,
    watchdog: Arc<RwLock<Watchdog>>,
    enforcer: Option<Arc<Enforcer>>,  // ← 新增，None if disabled/unsupported
}

pub async fn init_file_access(project_id: &str, project_root: &Path) -> ErgataiResult<()> {
    // ... existing code ...

    // 创建 Enforcer (Linux only, 失败则 fail-open 降级)
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

## 5. 事件循环核心逻辑

### 5.1 主循环伪代码

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

### 5.2 关键问题：读/写意图区分

fanotify `FAN_OPEN_PERM` 事件 **不暴露 open flags**。这是核心挑战。

**解决方案演进**：

| 方案 | 实现复杂度 | 准确性 | 推荐度 |
|------|-----------|--------|--------|
| A: 只拦截 WRITE lock holder 冲突 | 低 | 中（防并发冲突，不防无锁写入） | ★★★★ 短期 |
| B: FAN_CLOSE_WRITE 事后补救 | 低 | 中（写完了才发现，可以回滚） | ★★★ 辅助 |
| C: eBPF 补充（bpf_openfile） | 高 | 高（能精确读 flags） | ★★★★★ 长期 |
| D: LSM 自定义模块 | 极高 | 最高 | ★★ 内核级 |

**推荐分阶段实现**：

**Phase 1（MVP, 本文重点）**:
```
FAN_OPEN_PERM + /proc/{pid}/fd/* inspection + 进程树分析
↓
拦截场景: agent A 持 WRITE lock on file.rs
         agent B (通过 shell `echo >> file.rs`) open(file.rs, O_WRONLY)
         → 内核暂停 B 的 open()
         → enforcer 查 lock table → 发现 holder = A
         → 返回 FAN_DENY → B 的 open() 失败，返回 EPERM
         → audit: "ENFORCED_WRITE_CONFLICT agent=B holder=A"
```

**Phase 2（增强）**:
```
FAN_CLOSE_WRITE + 文件 hash 比较 (类似现有 FileSystemWatcher)
↓
检测: 文件被修改但无 lock → 触发 SIGSTOP + 通知 + 自动回滚 (git checkout)
```

**Phase 3（终极）**:
```
eBPF bpf_openfile 程序拦截 openat2() 系统调用
↓
精确读取 open flags, 区分 O_RDONLY / O_WRONLY / O_RDWR
在 open 入口就能判断意图, 无需 /proc 二次查询
```

### 5.3 处理 open flags 的实用方案

由于 fanotify 无法直接读 open flags，我们使用一个 **混合策略**：

```rust
/// 对于 FAN_OPEN_PERM 事件, 我们尝试以下方法推断 open 意图:
///
/// 1. /proc/{pid}/cmdline: 检查进程正在执行的命令
///    - 如果 cmdline 包含 "cat", "less", "head" → 大概率是 READ
///    - 如果 cmdline 包含 "vim", "sed -i", "echo >>" → 大概率是 WRITE
///    - 不可靠 (进程可以改名, exec 后 cmdline 变化)
///
/// 2. /proc/{pid}/fd/* 扫描 (在 FAN_OPEN_PERM 时点):
///    - 列出进程已持有的 fd, 找到刚打开的那个 (最新 inode match)
///    - 读 /proc/{pid}/fdinfo/{fd} 获取 open flags
///    - 问题: FAN_OPEN_PERM 时 open 还没完成, fd 还没创建
///
/// 3. 实际最可行: 使用 FAN_CLOSE_WRITE + hash 对比:
///    - 记录每个被监控文件的 pre-hash (在 FAN_OPEN 时)
///    - FAN_CLOSE_WRITE 触发时对比 post-hash
///    - 如果 hash 变化且无 lock → 违规 → 回滚 (git checkout) + 审计
///
/// 4. 或者使用 fanotify 的 FAN_MODIFY 事件:
///    - FAN_MODIFY 在 pwrite/write 时触发 (非权限事件)
///    - 可以配合 FAN_CLOSE_WRITE 实现 "检测到修改"
///
/// 5. 最佳折中: 拦截所有 FAN_OPEN_PERM, 对 "可能是写" 的 open
///    进行 lock 检查, 其余直接 ALLOW:
///    - 如果 file 已有 WRITE/ADMIN lock 且 caller 不是 holder → DENY
///    - 如果 file 无 lock → ALLOW (但仍可通过 FAN_CLOSE_WRITE 监测)
///    - 这样至少能防止 "两个 agent 同时写一个文件" 的冲突
```

---

## 6. NATS 事件集成

### 6.1 新增事件类型

```rust
// ergatai-nats/src/events.rs — 新增

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

### 6.2 发布逻辑

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

## 7. 与 AgentRuntime 的集成

### 7.1 PID 注册钩子

在 `RmuxBackend::discover_agents()` 完成后，通知 PidResolver 刷新：

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

    // ✨ 新增：通知 PidResolver 刷新
    if new_count > 0 {
        if let Some(resolver) = get_pid_resolver() {
            resolver.refresh().await?;
        }
    }

    Ok(new_count)
}
```

### 7.2 全局 PidResolver

```rust
// pid_resolver.rs — 全局单例

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

## 8. 测试策略

### 8.1 单元测试（不需要 root）

```rust
#[cfg(test)]
mod tests {
    // 测试 PidResolver 的 /proc 解析逻辑
    // 测试 decision cache 的 LRU 行为
    // 测试 EnforcerConfig 的 exclude_paths 匹配
    // 测试 lock_manager.get_write_lock_holder()

    #[tokio::test]
    async fn test_pid_resolver_walks_ancestors() {
        // 在测试进程中 fork 一个子进程
        // 注册父进程为 agent-1
        // 验证子进程的 resolve() 返回 agent-1
    }

    #[tokio::test]
    async fn test_decision_cache_expires() {
        // 缓存一个 decision, 等待 TTL 过期
        // 验证 cache miss
    }
}
```

### 8.2 集成测试（需要 Linux + root）

```rust
#[cfg(all(test, target_os = "linux"))]
mod integration_tests {
    // 需要 #[ignore] 默认，手动运行
    // 或检测 CAP_SYS_ADMIN capability

    #[tokio::test]
    #[ignore]  // requires root
    async fn test_enforcer_denies_unauthorized_write() {
        // 1. 启动 enforcer on temp dir
        // 2. agent-1 获取 WRITE lock on "file.txt"
        // 3. 模拟 agent-2 的进程 open("file.txt", O_WRONLY)
        // 4. 验证 open() 返回 EPERM
        // 5. 验证 audit_log 包含 ENFORCED_WRITE_CONFLICT
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_allows_lock_holder() {
        // agent-1 持有 lock → agent-1 的进程可以 open
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_allows_non_agent_process() {
        // 非 agent 的 PID 不被拦截
    }

    #[tokio::test]
    #[ignore]
    async fn test_enforcer_fail_open_on_lock_db_error() {
        // 模拟 SQLite 错误 → 验证 fail-open
    }
}
```

### 8.3 CI 考虑

```yaml
# GitHub Actions
- name: fanotify integration tests
  if: runner.os == 'Linux'
  run: cargo test -p ergatai-lock -- --ignored fanotify
  # 需要 --privileged or CAP_SYS_ADMIN
```

---

## 9. 风险与限制

### 9.1 平台限制

| 问题 | 影响 | 缓解 |
|------|------|------|
| Linux-only | macOS/Windows 无法使用 | `#[cfg(target_os = "linux")]`，其他平台保持 FileSystemWatcher |
| 需要 root/CAP_SYS_ADMIN | 普通用户无法启用 | 文档说明；或 setuid wrapper |
| 内核版本要求 | FAN_REPORT_FID 需 5.1+，FAN_RENAME 需 5.17+ | 检测内核版本，降级使用基础 FAN_OPEN_PERM |

### 9.2 兼容性

| 场景 | 行为 | 备注 |
|------|------|------|
| Docker container | fanotify 默认在 container 内可用 | 需要 `--privileged` 或 `CAP_SYS_ADMIN` |
| OverlayFS | FID 模式可能不支持 | 需要测试；可回退到 non-FID 模式 |
| NFS/network FS | fanotify 不支持 | 只监控本地路径 |
| btrfs/ZFS | 应该可以工作 | 建议测试覆盖 |
| 多挂载命名空间 | fanotify 是 per-mount | 需为每个挂载点 mark |

### 9.3 性能影响

**热路径分析**（每次文件 open）：
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

**影响评估**：
- 对普通开发工作流：无可感知延迟（<10μs per open）
- 对高频 I/O（如 cargo build）：可能增加 1-5% 总时间
- 建议：对 `target/` 目录默认 exclude

### 9.4 故障模式

| 故障 | 默认行为 | 可配置 |
|------|---------|--------|
| Enforcer panic | fail-open（允许所有） | `fail_closed: true` 反转 |
| SQLite 不可用 | fail-open | `fail_closed: true` 反转 |
| NATS 不可用 | 继续工作（仅本地审计） | ✓ |
| PidResolver stale | 拒绝新 agent（直到 refresh） | 缩短 refresh_interval |
| fanotify 初始化失败 | 降级到 FileSystemWatcher 模式 | ✓ |

### 9.5 安全风险

| 风险 | 缓解 |
|------|------|
| Agent 通过 `su`/`sudo` 切换到其他用户绕过 PID 映射 | 检查 /proc/{pid}/uid，非 agent uid → deny |
| Agent 通过 `nsenter` 切换命名空间 | fanotify 是 mount namespace 隔离的 |
| Agent 直接调用 syscall（绕过 glibc） | fanotify 在内核层拦截，无法绕过 |
| Agent 修改 enforcer 自身代码 | 通过 .ergatai/ 排除 + 锁保护 |

---

## 10. 实施路线图

### Phase 1: MVP（2-3 周）
- [ ] 提取 `pid_resolver.rs` 从 RmuxBackend
- [ ] 实现 `enforcer.rs` 基础框架（fanotify mark + event loop）
- [ ] 实现 "holder conflict detection"（拦截已被其他 agent 锁定的写操作）
- [ ] 集成到 `manager.rs` + `lib.rs`
- [ ] 单元测试 + 手动集成测试
- [ ] NATS 事件发布

### Phase 2: 增强（1-2 周）
- [ ] `FAN_CLOSE_WRITE` 事后补救（无锁写入检测 + 自动 git checkout 回滚）
- [ ] `get_write_lock_holder()` 接口
- [ ] 决策缓存优化
- [ ] `enforcer_metrics` 可观测性（Prometheus 指标）
- [ ] 配置 hot-reload（watch config file）

### Phase 3: 精确意图检测（2-3 周）
- [ ] eBPF 补充（精确读 open flags）
- [ ] 或者：使用 `seccomp-bpf` 拦截 openat
- [ ] 区分 READ / WRITE / CREATE / DELETE 操作
- [ ] 实现 "must hold lock to write" 强策略

### Phase 4: 生产化（ongoing）
- [ ] CI 集成（privileged test runner）
- [ ] 性能基准测试
- [ ] 文档 + 用户指南
- [ ] 渐进式发布（feature flag: `enforcer.enabled`）

---

## 11. 关键决策记录 (ADR)

### ADR-1: 选择 fanotify 而非 inotify
**决策**: 使用 fanotify
**理由**:
- inotify 只能事后通知（无 permission event）
- fanotify 提供 FAN_OPEN_PERM 同步拦截
- fanotify 支持全局 PID 信息（inotify 只有 wd）
- fanotify 支持 FAN_REPORT_FID（基于 inode，无需 path watch）

### ADR-2: fail-open vs fail-closed 默认策略
**决策**: 默认 fail-open
**理由**:
- fail-closed 会阻塞所有开发工作流当 enforcer 出错
- ergatai 是开发工具，不是安全产品
- 高级用户可配置 `fail_closed: true` 切换

### ADR-3: 分阶段实现 vs 一步到位
**决策**: 分阶段（Phase 1 先做冲突检测，Phase 3 做精确意图）
**理由**:
- Phase 1 已能解决最严重的 "并发写冲突" 问题
- eBPF 集成复杂度高，需要内核适配
- 渐进式发布降低风险

### ADR-4: PidResolver 作为独立模块
**决策**: 从 RmuxBackend 提取为共享模块
**理由**:
- fanotify enforcer 需要相同的 PID → agent_id 映射
- 未来可能用于其他功能（资源隔离、网络策略）
- 避免在 RmuxBackend 中耦合 fanotify 逻辑

---

## 12. 总结

本设计基于 Linux fanotify 的 `FAN_OPEN_PERM` 权限事件，为 Ergatai 提供 **内核级强制文件锁**。核心创新是：

1. **PID → agent_id 解析器**：复用 RmuxBackend 已有的 `/proc` 分析逻辑
2. **热路径缓存**：保证 <10μs 的响应延迟
3. **fail-open 策略**：enforcer 失败不影响开发工作流
4. **分阶段实施**：先做最有价值的冲突检测，再迭代增强

**Phase 1 的覆盖范围**：
- ✅ 防止两个 agent 同时写同一文件
- ✅ 告诉用户 "文件被 agent-X 锁定"（而不是静默失败）
- ✅ 审计日志完整记录所有拦截事件
- ❌ 不能防止 "无锁写入"（Phase 2/3 解决）
- ❌ 仅 Linux，需要 root/CAP_SYS_ADMIN

**架构兼容性**：
- 与现有 `FileSystemWatcher` 互补（fanotify 事前拦截 + notify 事后检测）
- 与 `FileLockManager` 共享 SQLite 数据库
- 通过 NATS 发布事件供 UI/dashboard 消费
